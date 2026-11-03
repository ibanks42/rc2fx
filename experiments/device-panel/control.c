#define _GNU_SOURCE
#include "control.h"
#include <errno.h>
#include <inttypes.h>
#include <pthread.h>
#include <sched.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#define MAX_REPLY (32u * 1024u)
static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t wake = PTHREAD_COND_INITIALIZER;
static struct UiSnapshot current;
static struct UiCommand pending;
static bool has_pending;
static _Atomic bool visible, revoked;
static char socket_name[sizeof(((struct sockaddr_un *)0)->sun_path)];

static uint64_t get64(const unsigned char *p) {
    uint64_t v=0;for(unsigned i=0;i<8;++i)v=(v<<8)|p[i];return v;
}
static unsigned get16(const unsigned char *p) {return (unsigned)p[0]*256+p[1];}
static void put64(unsigned char *p,uint64_t v) {for(unsigned i=0;i<8;++i)p[7-i]=(unsigned char)(v>>(i*8));}
static bool send_all(int fd,const void *buffer,size_t size) {
    const unsigned char *p=buffer;
    while(size) {ssize_t n=send(fd,p,size,MSG_NOSIGNAL);if(n<0&&errno==EINTR)continue;if(n<=0)return false;p+=n;size-=(size_t)n;}return true;
}
static bool recv_all(int fd,void *buffer,size_t size) {
    unsigned char *p=buffer;
    while(size) {ssize_t n=recv(fd,p,size,0);if(n<0&&errno==EINTR)continue;if(n<=0)return false;p+=n;size-=(size_t)n;}return true;
}
static int connect_controller(void) {
    int fd=socket(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0);if(fd<0)return -1;
    struct sockaddr_un address={.sun_family=AF_UNIX};
    memcpy(address.sun_path,socket_name,strlen(socket_name)+1);
    struct timeval wait={.tv_sec=2};
    if(setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&wait,sizeof wait)!=0
       ||setsockopt(fd,SOL_SOCKET,SO_SNDTIMEO,&wait,sizeof wait)!=0
       ||connect(fd,(const struct sockaddr *)&address,sizeof address)!=0){close(fd);return -1;}
    return fd;
}
static void display_string(char *out,size_t capacity,const unsigned char *in,size_t n) {
    size_t w=0;
    for(size_t i=0;i<n&&w+1<capacity;++i) {
        unsigned c=in[i];
        if(c>=128) {if((c&0xc0)!=0x80)out[w++]='?';continue;}
        if(c>='a'&&c<='z')c-=32;
        out[w++]=(c>=32&&c<127)?(char)c:' ';
    }
    out[w]='\0';
}
static bool decode(const unsigned char *reply,size_t n,struct UiSnapshot *out,unsigned *status) {
    if(n<28||memcmp(reply,"RCUI",4)||reply[4]!=1||(reply[5]&~63u))return false;
    *status=get16(reply+6);
    unsigned count=get16(reply+24),message=get16(reply+26);
    if(count>UI_MAX_PRESETS||message>512||message>n-28)return false;
    memset(out,0,sizeof *out);
    out->connected=true;out->known=(reply[5]&1)!=0;out->enabled=(reply[5]&2)!=0;
    out->ready=(reply[5]&4)!=0;out->applied=(reply[5]&8)!=0;
    out->can_disable=(reply[5]&16)!=0;out->can_enable=(reply[5]&32)!=0;
    out->revision=get64(reply+8);out->generation=get64(reply+16);out->count=count;
    display_string(out->message,sizeof out->message,reply+28,message);
    size_t cursor=28+message;
    for(unsigned i=0;i<count;++i) {
        if(n-cursor<2)return false;
        unsigned id=reply[cursor++],name=reply[cursor++];
        if(id==0||id>48||name>128||id+name>n-cursor)return false;
        for(unsigned j=0;j<id;++j) {unsigned c=reply[cursor+j];if(!((c>='a'&&c<='z')||(c>='0'&&c<='9')||c=='-'))return false;}
        memcpy(out->presets[i].id,reply+cursor,id);out->presets[i].id[id]='\0';cursor+=id;
        display_string(out->presets[i].name,sizeof out->presets[i].name,reply+cursor,name);cursor+=name;
    }
    return cursor==n&&*status>=100&&*status<600;
}
static bool rpc(int fd,const struct UiCommand *command,struct UiSnapshot *out,unsigned *status) {
    unsigned char packet[4+24+48]={0};size_t id=strnlen(command->id,sizeof command->id);
    if(id>48||command->op>2)return false;
    size_t length=24+id;
    packet[0]=(unsigned char)(length>>24);packet[1]=(unsigned char)(length>>16);
    packet[2]=(unsigned char)(length>>8);packet[3]=(unsigned char)length;
    memcpy(packet+4,"RCUI",4);packet[8]=1;packet[9]=(unsigned char)command->op;
    packet[10]=command->enabled?1:0;packet[11]=(unsigned char)id;
    put64(packet+12,command->revision);put64(packet+20,command->op==1?0:command->generation);memcpy(packet+28,command->id,id);
    if(!send_all(fd,packet,4+length))return false;
    struct timeval reply_wait={.tv_sec=command->op?17:2};
    if(setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&reply_wait,sizeof reply_wait)!=0)return false;
    unsigned char head[4];if(!recv_all(fd,head,4))return false;
    size_t n=(size_t)head[0]<<24|(size_t)head[1]<<16|(size_t)head[2]<<8|head[3];
    if(n<28||n>MAX_REPLY)return false;
    unsigned char reply[MAX_REPLY];
    return recv_all(fd,reply,n)&&decode(reply,n,out,status);
}
static void *worker(void *unused) {
    (void)unused;int fd=-1;char last_error[160]={0};
    cpu_set_t cpus;CPU_ZERO(&cpus);CPU_SET(0,&cpus);
    (void)pthread_setaffinity_np(pthread_self(),sizeof cpus,&cpus);
    (void)setpriority(PRIO_PROCESS,(id_t)syscall(SYS_gettid),10);
    for(;;) {
        struct UiCommand command={0};bool mutation=false;
        pthread_mutex_lock(&mutex);
        if(has_pending){command=pending;has_pending=false;mutation=true;}
        pthread_mutex_unlock(&mutex);
        if(mutation&&atomic_load_explicit(&revoked,memory_order_acquire)) {
            command=(struct UiCommand){0};mutation=false;
            pthread_mutex_lock(&mutex);current.busy=has_pending;pthread_mutex_unlock(&mutex);
        }
        if(fd<0)fd=connect_controller();
        struct UiSnapshot next;unsigned status=0;
        bool ok=fd>=0&&rpc(fd,&command,&next,&status);
        if(mutation) {
            if(ok)fprintf(stderr,"RC2FX UI: %s reply status=%u revision=%" PRIu64 " engine-confirmed=%u\n",
                command.op==1?"preset apply":command.enabled?"processing ON":"processing OFF",status,next.revision,next.applied?1u:0u);
            else fprintf(stderr,"RC2FX UI: mutation reply lost; outcome unknown; not replayed\n");
        }
        pthread_mutex_lock(&mutex);
        if(!ok) {
            if(fd>=0)close(fd);
            fd=-1;
            current.connected=false;current.ready=false;current.can_enable=false;current.can_disable=false;current.applied=false;current.busy=has_pending;
            snprintf(current.message,sizeof current.message,"%s",mutation?"OUTCOME UNKNOWN - REFRESH BEFORE RETRY":"CONTROLLER OFFLINE");
            if(mutation)snprintf(last_error,sizeof last_error,"%s",current.message);
        } else if(status>=400) {
            snprintf(last_error,sizeof last_error,"%s",status==409?"SETTINGS CHANGED - REVIEW AND TRY AGAIN":next.message);
            current.ready=false;current.can_enable=false;current.can_disable=false;current.busy=has_pending;
            snprintf(current.message,sizeof current.message,"%s",last_error);
        } else {
            next.busy=has_pending;current=next;
            if(mutation)last_error[0]='\0';
            if(last_error[0])snprintf(current.message,sizeof current.message,"%s",last_error);
        }
        /* Mutations are consumed once, NEVER replayed after disconnect/lost
         * replies. The next operation is a read-only refreshed snapshot. */
        struct timespec until;clock_gettime(CLOCK_REALTIME,&until);
        until.tv_nsec+=atomic_load_explicit(&visible,memory_order_relaxed)?250000000:750000000;
        if(until.tv_nsec>=1000000000){++until.tv_sec;until.tv_nsec-=1000000000;}
        if(!has_pending)pthread_cond_timedwait(&wake,&mutex,&until);
        pthread_mutex_unlock(&mutex);
    }
    return NULL;
}
bool ui_control_start(const char *path) {
    if(!path||path[0]!='/'||strlen(path)>=sizeof socket_name)return false;
    memcpy(socket_name,path,strlen(path)+1);
    pthread_t thread;
    if(pthread_create(&thread,NULL,worker,NULL)!=0)return false;
    pthread_detach(thread);return true;
}
bool ui_control_snapshot(struct UiSnapshot *out) {
    if(pthread_mutex_trylock(&mutex)!=0)return false;
    *out=current;pthread_mutex_unlock(&mutex);return true;
}
bool ui_control_submit(const struct UiCommand *command) {
    if(command->op<1||command->op>2||pthread_mutex_trylock(&mutex)!=0)return false;
    bool allowed=command->op==1?current.ready:command->enabled?current.can_enable:current.can_disable;
    bool accept=!atomic_load_explicit(&revoked,memory_order_acquire)&&current.connected&&allowed&&!current.busy&&!has_pending
        &&command->revision==current.revision&&command->generation==current.generation;
    if(accept){pending=*command;has_pending=true;current.busy=true;pthread_cond_signal(&wake);}
    else if(current.connected&&!current.busy)
        snprintf(current.message,sizeof current.message,"SETTINGS CHANGED - REVIEW AND TRY AGAIN");
    pthread_mutex_unlock(&mutex);return accept;
}
void ui_control_visible(bool value) {atomic_store_explicit(&visible,value,memory_order_relaxed);}
void ui_control_revoke(void) {atomic_store_explicit(&revoked,true,memory_order_release);}
