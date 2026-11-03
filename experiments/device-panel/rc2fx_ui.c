/* RODECaster Pro II on-screen RC2FX panel (unfinished experiment).
 * GNU ld.so audit module for the exact 1.7.7 rc_gui_juce executable.
 *
 * The vendor paints a PRIVATE shadow of its V4L2 OUTPUT mapping. Immediately
 * before its original QBUF, under its original repaint mutex, we copy the
 * shadow into the real driver mapping and draw the panel there. Vendor dirty
 * paints never see our pixels. A queued/failed-to-dequeue buffer is NEVER
 * written again. No foreign JUCE widgets, code patches, DRM-master takeover or second
 * evdev reader. A launcher opens/closes the panel in-process. Display changes
 * are acknowledged by the actual DRM page-flip event before touch ownership
 * can be returned to stock. A supervisor still bounds development trials.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <link.h>
#include <linux/input.h>
#include <linux/videodev2.h>
#include <pthread.h>
#include <poll.h>
#include <sched.h>
#include <sys/syscall.h>
#include <sys/resource.h>
#include <sys/select.h>
#include <xf86drm.h>
#include "control.h"
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#define EXPORT __attribute__((visibility("default")))
#define NOINLINE __attribute__((noinline))
#define GUI_SIZE 19761224
#define WIDTH 1280u
#define HEIGHT 720u
#define PIXELS (WIDTH * HEIGHT * 4u)
#define RAW_TOUCH_HEIGHT 720

/* In this pinned executable, each is the instruction AFTER the imported call.
 * Identity is additionally checked with the full executable SHA-256 below. */
#define SITE_FMT       0x7cfe00u
#define SITE_QUERY     0x7d002cu
#define SITE_MMAP      0x7d004cu
#define SITE_QBUF      0x7d0638u
#define SITE_DQBUF     0x7d0820u
#define SITE_CAPTURE_DQ 0x7d080cu
#define SITE_SELECT    0x7cfaccu
#define SITE_COMMIT    0x7d0ae8u
#define SITE_HANDLE    0x7d0c28u
#define SITE_READ1     0x7e0760u
#define SITE_READ2     0x7e07d0u

static uintptr_t main_base;
static bool main_seen, enabled;
static pid_t gui_pid;
static bool is_gui(void) { return enabled && getpid()==gui_pid; }
static _Atomic bool shadow_live;
static _Atomic bool failed;
static _Atomic bool close_requested, first_presented;
static const char *close_path;
static int video_fd = -1;
static size_t source_length;
static off_t source_offset;
static bool format_ok, query_ok;
static uint8_t *driver_pixels, *paint_pixels;
/* These are used only on the original frame/IOCTL thread after initialization.
 * Allocation publishes before source QBUF; we never race vendor repainting:
 * the pinned conversion caller holds the vendor's shared repaint mutex. */
static bool queued;
static unsigned long frames;
static int touch_fd = -1;
static dev_t touch_device;
static ino_t touch_inode;
static int raw_x, raw_y;
static bool have_x, have_y;
static _Atomic bool retiring_mapping;
/* Only the original DRM frame thread writes these conversion fields. */
static bool capture_dequeued, conversion_ready;
static uint64_t source_view;
static _Atomic uint64_t wanted_view;
static _Atomic uint64_t presented_view = UINT64_MAX;
static pthread_mutex_t input_mutex = PTHREAD_MUTEX_INITIALIZER;
enum InputOwner { INPUT_DRAIN, INPUT_NONE, INPUT_STOCK, INPUT_PANEL };
static enum InputOwner input_owner = INPUT_DRAIN;
static bool contact_pressed, gesture_ended;
struct Contact { int id,x,y; };
static struct Contact contacts[32];
static unsigned current_slot;
static bool key_touch,key_left;
static struct input_event event_frame[256], replay[256];
static size_t frame_count,replay_count,replay_cursor;
static unsigned gesture_action;
static int gesture_slot=-1, gesture_tracking=-1;
static struct UiCommand gesture_command;
static char selected_id[49];
static unsigned page;
struct UiScene {
    struct UiSnapshot state;
    uint64_t view, epoch, serial;
    unsigned page;
    char selected[49];
};
/* A scene is published with its real flip, not when merely painted/queued. */
static struct UiScene drawing_scene, displayed_scene;
enum TicketState { TICKET_FREE, TICKET_QUEUED, TICKET_COMPLETE, TICKET_READING };
struct FlipTicket { _Atomic unsigned state; struct UiScene scene; };
static _Atomic uint64_t submitted_epoch, acknowledged_epoch;
static uint64_t next_epoch, next_serial;
static struct FlipTicket tickets[4];
static _Thread_local void (*previous_flip)(int,unsigned,unsigned,unsigned,void *);
static _Thread_local void (*previous_flip2)(int,unsigned,unsigned,unsigned,unsigned,void *);

static int (*vendor_ioctl)(int, unsigned long, uintptr_t);
static void *(*vendor_mmap)(void *, size_t, int, int, int, off_t);
static int (*vendor_munmap)(void *, size_t);
static int (*vendor_close)(int);
static ssize_t (*vendor_read)(int, void *, size_t);
static int (*vendor_select)(int,fd_set *,fd_set *,fd_set *,struct timeval *);
static int (*vendor_commit)(int,void *,uint32_t,void *);
static int (*vendor_handle_event)(int,drmEventContextPtr);
static void (*vendor_condition_wait)(void *,void *);
static void (*vendor_condition_destroy)(void *);
static _Atomic long renderer_tid;
static pthread_mutex_t event_mutex=PTHREAD_MUTEX_INITIALIZER;
static void *renderer_event;
static bool pump_started;
static void (*signal_renderer)(void *);
/* ld.so audit modules have a separate libc/TLS namespace. errno must refer
 * to the caller's libc, NOT this module's errno, after a forwarded call. */
static int *(*vendor_errno)(void);
static int saved_vendor_errno(void) { return vendor_errno ? *vendor_errno() : 0; }
static void restore_vendor_errno(int value) { if(vendor_errno)*vendor_errno()=value; }

static uintptr_t call_site(void *address) {
    return (uintptr_t)__builtin_extract_return_addr(address) - main_base;
}

/* Standalone SHA-256 so admission needs no OpenSSL or libjuce dependency. */
struct sha256 { uint32_t h[8]; uint64_t bytes; uint8_t block[64]; size_t used; };
static uint32_t rotr(uint32_t x, unsigned n) { return (x >> n) | (x << (32 - n)); }
static void sha_block(struct sha256 *s, const uint8_t *p) {
    static const uint32_t k[64] = {
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2
    };
    uint32_t w[64];
    for (unsigned i = 0; i < 16; ++i)
        w[i] = (uint32_t)p[i*4] << 24 | (uint32_t)p[i*4+1] << 16
             | (uint32_t)p[i*4+2] << 8 | p[i*4+3];
    for (unsigned i = 16; i < 64; ++i) {
        uint32_t a = w[i-15], b = w[i-2];
        w[i] = w[i-16] + (rotr(a,7)^rotr(a,18)^(a>>3)) + w[i-7]
             + (rotr(b,17)^rotr(b,19)^(b>>10));
    }
    uint32_t a=s->h[0],b=s->h[1],c=s->h[2],d=s->h[3];
    uint32_t e=s->h[4],f=s->h[5],g=s->h[6],h=s->h[7];
    for (unsigned i=0;i<64;++i) {
        uint32_t t1=h+(rotr(e,6)^rotr(e,11)^rotr(e,25))+((e&f)^(~e&g))+k[i]+w[i];
        uint32_t t2=(rotr(a,2)^rotr(a,13)^rotr(a,22))+((a&b)^(a&c)^(b&c));
        h=g;g=f;f=e;e=d+t1;d=c;c=b;b=a;a=t1+t2;
    }
    s->h[0]+=a;s->h[1]+=b;s->h[2]+=c;s->h[3]+=d;
    s->h[4]+=e;s->h[5]+=f;s->h[6]+=g;s->h[7]+=h;
}
static void sha_init(struct sha256 *s) {
    *s=(struct sha256){.h={0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
                                0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19}};
}
static void sha_update(struct sha256 *s,const uint8_t *p,size_t n) {
    s->bytes+=n;
    while(n) {
        size_t part=64-s->used;if(part>n)part=n;
        memcpy(s->block+s->used,p,part);s->used+=part;p+=part;n-=part;
        if(s->used==64){sha_block(s,s->block);s->used=0;}
    }
}
static void sha_final(struct sha256 *s,uint8_t out[32]) {
    uint64_t bits=s->bytes*8;
    uint8_t one=0x80,zero=0;
    sha_update(s,&one,1);
    while(s->used!=56)sha_update(s,&zero,1);
    uint8_t tail[8];for(unsigned i=0;i<8;++i)tail[7-i]=(uint8_t)(bits>>(i*8));
    sha_update(s,tail,8);
    for(unsigned i=0;i<32;++i)out[i]=(uint8_t)(s->h[i/4]>>(24-(i%4)*8));
}
static bool admitted_executable(void) {
    static const uint8_t expected[32]={
        0xaa,0x92,0xa9,0x2d,0x38,0x4b,0xdb,0x24,0xbc,0x1c,0xdb,0x01,0x61,0x5a,0xc2,0x80,
        0xd4,0xd4,0x86,0x7e,0x66,0x11,0x58,0xdf,0x5c,0x7f,0x86,0xff,0x98,0xb6,0x05,0xca};
    int fd=open("/proc/self/exe",O_RDONLY|O_CLOEXEC);
    if(fd<0)return false;
    struct stat st;
    if(fstat(fd,&st)!=0||st.st_size!=GUI_SIZE){close(fd);return false;}
    struct sha256 s;sha_init(&s);uint8_t buffer[16384],digest[32];ssize_t n;
    while((n=read(fd,buffer,sizeof buffer))>0)sha_update(&s,buffer,(size_t)n);
    close(fd);if(n<0)return false;
    sha_final(&s,digest);return memcmp(expected,digest,32)==0;
}

static void note(const char *message) {
    /* Only initialization, first frame, Close or fault, not per-frame logging. */
    static const char prefix[]="RC2FX UI: ";
    (void)write(STDERR_FILENO,prefix,sizeof prefix-1);
    (void)write(STDERR_FILENO,message,strlen(message));
    (void)write(STDERR_FILENO,"\n",1);
}
static void mark_session(const char *name) {
    if(!close_path)return;
    const char *slash=strrchr(close_path,'/');
    if(!slash||strlen(close_path)>200)return;
    char path[240];
    int n=snprintf(path,sizeof path,"%.*s/%s",(int)(slash-close_path),close_path,name);
    if(n<=0||(size_t)n>=sizeof path)return;
    int fd=open(path,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC,0600);
    if(fd>=0){(void)write(fd,"1\n",2);close(fd);}
}
static void request_exit(const char *reason) {
    if(atomic_exchange_explicit(&close_requested,true,memory_order_acq_rel))return;
    note(reason);
    // Only private RAM files; never vendor settings or boot configuration.
    mark_session("close");
}
static void fault(const char *reason) {
    ui_control_revoke();
    if(!atomic_exchange_explicit(&failed,true,memory_order_acq_rel))request_exit(reason);
}

/* Original 5x7 lettering. Neutral greys/white avoid assuming V4L2 BA24 color
 * channels match DRM XR24; both byte orders render equal R/G/B correctly. */
static const uint8_t font[26][7]={
 {14,17,17,31,17,17,17},{30,17,17,30,17,17,30},{14,17,16,16,16,17,14},
 {30,17,17,17,17,17,30},{31,16,16,30,16,16,31},{31,16,16,30,16,16,16},
 {14,17,16,23,17,17,14},{17,17,17,31,17,17,17},{31,4,4,4,4,4,31},
 {7,2,2,2,18,18,12},{17,18,20,24,20,18,17},{16,16,16,16,16,16,31},
 {17,27,21,21,17,17,17},{17,25,25,21,19,19,17},{14,17,17,17,17,17,14},
 {30,17,17,30,16,16,16},{14,17,17,17,21,18,13},{30,17,17,30,20,18,17},
 {15,16,16,14,1,1,30},{31,4,4,4,4,4,4},{17,17,17,17,17,17,14},
 {17,17,17,17,17,10,4},{17,17,17,21,21,21,10},{17,17,10,4,10,17,17},
 {17,17,10,4,4,4,4},{31,1,2,4,8,16,31}};
static void rectangle(unsigned x,unsigned y,unsigned w,unsigned h,uint8_t gray) {
    if(x>WIDTH||y>HEIGHT||w>WIDTH-x||h>HEIGHT-y)return;
    for(unsigned row=y;row<y+h;++row)for(unsigned col=x;col<x+w;++col) {
        uint8_t *p=driver_pixels+((size_t)row*WIDTH+col)*4;
        p[0]=gray;p[1]=gray;p[2]=gray;p[3]=255;
    }
}
static void text(unsigned x,unsigned y,const char *s,unsigned scale,uint8_t gray) {
    static const uint8_t digits[10][7]={
        {14,17,19,21,25,17,14},{4,12,4,4,4,4,14},{14,17,1,2,4,8,31},
        {30,1,1,14,1,1,30},{2,6,10,18,31,2,2},{31,16,16,30,1,1,30},
        {14,16,16,30,17,17,14},{31,1,2,4,8,8,8},{14,17,17,14,17,17,14},{14,17,17,15,1,1,14}};
    static const uint8_t dash[7]={0,0,0,31,0,0,0},dot[7]={0,0,0,0,0,12,12};
    static const uint8_t question[7]={14,17,1,2,4,0,4};
    for(;*s;++s,x+=6*scale) {
        const uint8_t *g=(*s>='0'&&*s<='9')?digits[*s-'0']:
            (*s>='A'&&*s<='Z')?font[*s-'A']:*s=='-'?dash:*s=='.'?dot:*s=='?'?question:NULL;
        if(!g)continue;
        for(unsigned row=0;row<7;++row)for(unsigned col=0;col<5;++col)
            if(g[row]&(1u<<(4-col)))rectangle(x+col*scale,y+row*scale,scale,scale,gray);
    }
}
static bool selected_exists(const struct UiScene *scene) {
    for(unsigned i=0;i<scene->state.count;++i)
        if(strcmp(scene->selected,scene->state.presets[i].id)==0)return true;
    return false;
}
static void draw_panel(void) {
    static struct UiSnapshot latest;
    (void)ui_control_snapshot(&latest);
    drawing_scene.state=latest;
    if(pthread_mutex_trylock(&input_mutex)==0) {
        // A view tag certifies these exact local contents. On contention keep
        // the old tag too; never ACK a requested selection/page we failed to
        // copy while its input handler was still publishing it.
        drawing_scene.view=atomic_load_explicit(&wanted_view,memory_order_acquire);
        drawing_scene.page=page;
        memcpy(drawing_scene.selected,selected_id,sizeof selected_id);
        pthread_mutex_unlock(&input_mutex);
    }
    source_view=drawing_scene.view;
    /* Empty top-bar space in the user's photograph, clear of clock, monitor,
     * Wi-Fi and the stock Settings button. This is always drawn last. */
    rectangle(600,14,150,52,190);rectangle(603,17,144,46,35);
    text(630,29,"RC2FX",3,255);
    if(!(source_view&1))return;
    const struct UiSnapshot *s=&drawing_scene.state;
    rectangle(240,100,800,580,22);
    rectangle(240,100,800,4,205);rectangle(240,676,800,4,205);
    rectangle(240,104,4,572,205);rectangle(1036,104,4,572,205);
    text(280,130,"RC2FX",5,255);
    const char *state=!s->connected?"CONTROLLER OFFLINE":s->busy?"APPLYING - PLEASE WAIT":
        !s->known?(s->can_disable?"PROCESSING ERROR":"PROCESSING PENDING"):s->enabled?"PROCESSING ON":"PROCESSING OFF";
    text(280,190,state,3,230);
    bool off=s->can_disable, toggle=s->connected&&(off||s->can_enable);
    rectangle(750,125,250,58,toggle&&!s->busy?70:35);
    text(780,145,off?"TURN OFF":"TURN ON",3,toggle&&!s->busy?255:110);
    text(280,230,s->connected&&s->applied&&!s->busy?"CONFIRMED BY AUDIO ENGINE":"WAITING FOR AUDIO CONFIRMATION",2,160);
    unsigned first=drawing_scene.page*5;
    if(first>=s->count)first=0;
    drawing_scene.page=first/5;
    char pages[40];snprintf(pages,sizeof pages,"PRESETS - PAGE %u OF %u",drawing_scene.page+1,s->count?(s->count+4)/5:1);
    text(280,248,pages,2,170);
    if(s->count==0)text(280,290,"NO SAVED PRESETS - SAVE ONE ON DESKTOP",2,180);
    for(unsigned row=0;row<5&&first+row<s->count;++row) {
        const struct UiPreset *preset=&s->presets[first+row];
        bool selected=strcmp(drawing_scene.selected,preset->id)==0;
        rectangle(280,270+row*50,720,44,selected?90:43);
        char label[51];snprintf(label,sizeof label,"%.50s",preset->name);
        text(296,284+row*50,label,2,selected?255:210);
    }
    rectangle(280,550,100,50,50);text(292,567,"PREV",3,210);
    rectangle(400,550,100,50,50);text(412,567,"NEXT",3,210);
    bool apply=s->ready&&!s->busy&&selected_exists(&drawing_scene);
    rectangle(580,550,210,50,apply?75:35);text(630,567,"APPLY",3,apply?255:110);
    rectangle(820,550,180,50,65);text(856,567,"CLOSE",3,255);
    char message[61];snprintf(message,sizeof message,"%.60s",s->message);
    text(280,625,message,2,170);
}

static NOINLINE void *ui_mmap(void *address,size_t size,int prot,int flags,int fd,off_t offset) {
    uintptr_t site=call_site(__builtin_return_address(0));
    void *mapped=vendor_mmap(address,size,prot,flags,fd,offset);
    int saved=saved_vendor_errno();
    if(is_gui()&&site==SITE_MMAP&&fd==video_fd&&format_ok&&query_ok
       &&!driver_pixels&&!retiring_mapping&&mapped!=MAP_FAILED&&size==source_length
       &&offset==source_offset&&size>=PIXELS&&size<=16u*1024u*1024u
       &&prot==(PROT_READ|PROT_WRITE)&&flags==MAP_SHARED&&address==NULL) {
        /* Different allocation: never expose the DMA-owned mapping to JUCE.
         * Keep both mapped until process exit; teardown is preview-scoped. */
        void *shadow=mmap(NULL,size,PROT_READ|PROT_WRITE,MAP_PRIVATE|MAP_ANONYMOUS,-1,0);
        if(shadow!=MAP_FAILED) {
            memcpy(shadow,mapped,size);
            driver_pixels=mapped;paint_pixels=shadow;
            atomic_store_explicit(&shadow_live,true,memory_order_release);
            note("V4L2 source shadow installed; panel will be drawn on the real queue path");
            restore_vendor_errno(saved);return shadow;
        }
        fault("source shadow allocation failed; returning to stock UI");
    } else if(is_gui()&&site==SITE_MMAP&&mapped!=MAP_FAILED) {
        fault("V4L2 mapping/format differs from qualified layout; returning to stock UI");
    }
    restore_vendor_errno(saved);return mapped;
}
static NOINLINE int ui_ioctl(int fd,unsigned long request,uintptr_t argument) {
    uintptr_t site=call_site(__builtin_return_address(0));
    bool copy=is_gui()&&atomic_load_explicit(&shadow_live,memory_order_acquire)
              &&fd==video_fd&&request==VIDIOC_QBUF&&site==SITE_QBUF;
    if(copy) {
        struct v4l2_buffer *b=(void *)argument;
        if(queued) {
            /* Crucial: timeout does NOT transfer a queued buffer back to CPU.
             * Do not copy/restore/write or submit another buffer in that state.
             * Supervisor terminates this preview, closing the driver normally. */
            fault("previous V4L2 source is still queued; no memory write; returning to stock UI");
            restore_vendor_errno(EBUSY);return -1;
        }
        if(!b||b->type!=V4L2_BUF_TYPE_VIDEO_OUTPUT||b->memory!=V4L2_MEMORY_MMAP||b->index!=0) {
            fault("unexpected output QBUF; returning to stock UI");restore_vendor_errno(EINVAL);return -1;
        }
        atomic_store_explicit(&renderer_tid,syscall(SYS_gettid),memory_order_release);
        source_view=atomic_load_explicit(&wanted_view,memory_order_acquire);
        capture_dequeued=false;conversion_ready=false;
        memcpy(driver_pixels,paint_pixels,source_length);
        if(!atomic_load_explicit(&failed,memory_order_acquire))draw_panel();
    }
    int result=vendor_ioctl(fd,request,argument);
    int saved=saved_vendor_errno();
    if(is_gui()&&result==0&&request==VIDIOC_S_FMT&&site==SITE_FMT&&argument) {
        struct v4l2_format *f=(void *)argument;
        format_ok=f->type==V4L2_BUF_TYPE_VIDEO_OUTPUT&&f->fmt.pix.width==WIDTH
             &&f->fmt.pix.height==HEIGHT&&f->fmt.pix.pixelformat==V4L2_PIX_FMT_ARGB32
             &&f->fmt.pix.bytesperline==WIDTH*4&&f->fmt.pix.sizeimage>=PIXELS;
        if(format_ok)video_fd=fd;
        else fault("driver did not accept the expected BA24 source format");
    }
    if(is_gui()&&result==0&&fd==video_fd&&request==VIDIOC_QUERYBUF&&site==SITE_QUERY&&argument) {
        struct v4l2_buffer *b=(void *)argument;
        query_ok=b->type==V4L2_BUF_TYPE_VIDEO_OUTPUT&&b->memory==V4L2_MEMORY_MMAP
             &&b->index==0&&b->length>=PIXELS&&b->length<=16u*1024u*1024u;
        if(query_ok){source_length=b->length;source_offset=(off_t)b->m.offset;}
    }
    if(copy) {
        if(result==0) {
            queued=true;
            if(++frames==1)note("first real V4L2 frame queued with RC2FX launcher");
        } else fault("V4L2 output queue failed; returning to stock UI");
    }
    if(is_gui()&&fd==video_fd&&request==VIDIOC_DQBUF&&site==SITE_CAPTURE_DQ) {
        const struct v4l2_buffer *b=(const void *)argument;
        capture_dequeued=result==0&&b&&b->type==V4L2_BUF_TYPE_VIDEO_CAPTURE&&b->index==0&&!(b->flags&V4L2_BUF_FLAG_ERROR);
        if(!capture_dequeued)fault("capture dequeue failed; no presentation acknowledgement admitted");
    }
    if(is_gui()&&fd==video_fd&&request==VIDIOC_DQBUF&&site==SITE_DQBUF) {
        if(result==0&&argument) {
            const struct v4l2_buffer *b=(void *)argument;
            if(b->type==V4L2_BUF_TYPE_VIDEO_OUTPUT&&b->memory==V4L2_MEMORY_MMAP&&b->index==0&&!(b->flags&V4L2_BUF_FLAG_ERROR))
                {queued=false;conversion_ready=capture_dequeued;}
            else fault("unexpected source dequeue identity");
        } else fault("source dequeue failed; buffer left untouched");
    }
    if(is_gui()&&fd==video_fd&&request==VIDIOC_STREAMOFF&&result==0&&argument
       &&*(const int *)argument==V4L2_BUF_TYPE_VIDEO_OUTPUT)queued=false;
    restore_vendor_errno(saved);return result;
}
static bool touchscreen(int fd) {
    struct stat st,want;
    if(fstat(fd,&st)!=0||!S_ISCHR(st.st_mode))return false;
    if(fd==touch_fd&&st.st_rdev==touch_device&&st.st_ino==touch_inode)return true;
    if(stat("/dev/input/touchscreen0",&want)!=0||st.st_rdev!=want.st_rdev||st.st_ino!=want.st_ino)
        return false;
    touch_fd=fd;touch_device=st.st_rdev;touch_inode=st.st_ino;
    return true;
}
enum Action { ACTION_NONE, ACTION_LAUNCH, ACTION_CLOSE, ACTION_SELECT, ACTION_PREV, ACTION_NEXT, ACTION_APPLY, ACTION_TOGGLE };
static bool inside(int x,int y,int left,int top,int width,int height) {
    return x>=left&&x<left+width&&y>=top&&y<top+height;
}
static void consume_completions(void);
static unsigned contact_count(void) {
    unsigned n=0;for(unsigned i=0;i<32;++i)if(contacts[i].id>=0)++n;
    return n?n:(key_touch||key_left?1u:0u);
}
static bool touch_quiet(int fd) {
    unsigned char keys[(KEY_MAX+8)/8]={0};
    if(ioctl(fd,EVIOCGKEY(sizeof keys),keys)<0)return false;
    if((keys[BTN_TOUCH/8]&(1u<<(BTN_TOUCH%8)))||(keys[BTN_LEFT/8]&(1u<<(BTN_LEFT%8))))return false;
    struct input_absinfo slots;
    if(ioctl(fd,EVIOCGABS(ABS_MT_SLOT),&slots)==0) {
        if(slots.minimum!=0||slots.maximum<0||slots.maximum>=32)return false;
        int values[33]={ABS_MT_TRACKING_ID};
        size_t bytes=(size_t)(slots.maximum+2)*sizeof(int);
        if(ioctl(fd,EVIOCGMTSLOTS(bytes),values)<0)return false;
        for(int i=1;i<=slots.maximum+1;++i)if(values[i]>=0)return false;
        values[0]=ABS_MT_POSITION_X;
        if(ioctl(fd,EVIOCGMTSLOTS(bytes),values)<0)return false;
        for(int i=0;i<=slots.maximum;++i){contacts[i].id=-1;contacts[i].x=values[i+1];}
        values[0]=ABS_MT_POSITION_Y;
        if(ioctl(fd,EVIOCGMTSLOTS(bytes),values)<0)return false;
        for(int i=0;i<=slots.maximum;++i)contacts[i].y=values[i+1];
        if(slots.value<0||slots.value>slots.maximum)return false;
        current_slot=(unsigned)slots.value;
        raw_x=contacts[current_slot].x;raw_y=contacts[current_slot].y;
    } else {
        struct input_absinfo x,y;
        if(ioctl(fd,EVIOCGABS(ABS_X),&x)!=0||ioctl(fd,EVIOCGABS(ABS_Y),&y)!=0)return false;
        raw_x=x.value;raw_y=y.value;
    }
    // A snapshot is not an ordering barrier if records arrived during it.
    // No other code reads this FD; a final nonblocking readiness check means
    // future records start from the coordinates just snapshotted above.
    struct pollfd p={.fd=fd,.events=POLLIN};
    if(poll(&p,1,0)!=0)return false;
    key_touch=false;key_left=false;return true;
}
/* Must hold input_mutex. Both the empty read/select and the final snapshot
 * must follow the exact acknowledged content epoch, not just an open bit. */
static void release_barrier(int fd,uint64_t before) {
    uint64_t ack=atomic_load_explicit(&acknowledged_epoch,memory_order_acquire);
    if(!atomic_load_explicit(&failed,memory_order_acquire)&&!atomic_load_explicit(&retiring_mapping,memory_order_acquire)
       &&input_owner==INPUT_DRAIN&&frame_count==0&&replay_cursor==replay_count&&ack!=0&&ack==before
       &&ack==atomic_load_explicit(&submitted_epoch,memory_order_acquire)
       &&atomic_load_explicit(&presented_view,memory_order_acquire)==atomic_load_explicit(&wanted_view,memory_order_acquire)
       &&touch_quiet(fd)&&ack==atomic_load_explicit(&submitted_epoch,memory_order_acquire)) {
        input_owner=INPUT_NONE;contact_pressed=false;gesture_action=ACTION_NONE;
    }
}
static void request_layout(bool open) {
    uint64_t old=atomic_load_explicit(&wanted_view,memory_order_relaxed);
    if(old>UINT64_MAX-3){fault("UI view generation exhausted");return;}
    atomic_store_explicit(&wanted_view,((old+2)&~UINT64_C(1))|(open?1u:0u),memory_order_release);
    input_owner=INPUT_DRAIN;ui_control_visible(open);
}
static unsigned hit(const struct UiScene *scene,int x,int y,struct UiCommand *command) {
    memset(command,0,sizeof *command);
    command->revision=scene->state.revision;command->generation=scene->state.generation;
    if(inside(x,y,600,14,150,52))return ACTION_LAUNCH;
    if(!(scene->view&1))return ACTION_NONE;
    if(inside(x,y,820,550,180,50))return ACTION_CLOSE;
    if(inside(x,y,280,550,100,50))return ACTION_PREV;
    if(inside(x,y,400,550,100,50))return ACTION_NEXT;
    for(unsigned row=0;row<5;++row) {
        unsigned index=scene->page*5+row;
        if(index<scene->state.count&&inside(x,y,280,270+(int)row*50,720,44)) {
            memcpy(command->id,scene->state.presets[index].id,sizeof command->id);return ACTION_SELECT;
        }
    }
    if(!scene->state.connected||scene->state.busy)return ACTION_NONE;
    if(inside(x,y,750,125,250,58)&&(scene->state.can_disable||scene->state.can_enable)) {
        command->op=2;command->enabled=!scene->state.can_disable;return ACTION_TOGGLE;
    }
    if(inside(x,y,580,550,210,50)&&scene->state.ready&&selected_exists(scene)) {
        command->op=1;memcpy(command->id,scene->selected,sizeof command->id);return ACTION_APPLY;
    }
    return ACTION_NONE;
}
static void finish_gesture(int x,int y) {
    if(atomic_load_explicit(&failed,memory_order_acquire)||atomic_load_explicit(&retiring_mapping,memory_order_acquire))return;
    struct UiCommand released;
    unsigned action=hit(&displayed_scene,x,y,&released);
    if(action!=gesture_action)return;
    bool open=(atomic_load_explicit(&wanted_view,memory_order_acquire)&1)!=0;
    switch(action) {
    case ACTION_LAUNCH:request_layout(!open);break;
    case ACTION_CLOSE:request_layout(false);break;
    case ACTION_SELECT:
        if(strcmp(released.id,gesture_command.id)==0) {
            memcpy(selected_id,gesture_command.id,sizeof selected_id);request_layout(true);
        }break;
    case ACTION_PREV:
        page=displayed_scene.page?displayed_scene.page-1:0;request_layout(true);break;
    case ACTION_NEXT:
        if((displayed_scene.page+1)*5<displayed_scene.state.count) {
            page=displayed_scene.page+1;request_layout(true);
        }break;
    case ACTION_APPLY:case ACTION_TOGGLE:(void)ui_control_submit(&gesture_command);break;
    default:break;
    }
}
/* Buffer exactly one complete evdev report before deciding ownership. Current
 * kernel coordinates MUST NOT classify older queued contacts. Unchanged axes
 * are retained per slot from a snapshot taken at an empty-queue barrier. */
static void process_frame(void) {
    if(atomic_load_explicit(&failed,memory_order_acquire)||atomic_load_explicit(&retiring_mapping,memory_order_acquire)) {
        input_owner=INPUT_DRAIN;gesture_action=ACTION_NONE;return;
    }
    unsigned before=contact_count(),maximum=before,mt_starts=0;
    int first_new=-1,first_id=-1;
    bool began=false,replaced=false,key_lifted=false;
    for(size_t i=0;i<frame_count;++i) {
        const struct input_event *e=&event_frame[i];
        if(e->type==EV_ABS) {
            if(e->code==ABS_MT_SLOT) {
                if(e->value<0||e->value>=32){fault("invalid multitouch slot");input_owner=INPUT_DRAIN;return;}
                current_slot=(unsigned)e->value;
            } else if(e->code==ABS_MT_TRACKING_ID) {
                if(input_owner==INPUT_PANEL&&(int)current_slot==gesture_slot
                   &&contacts[current_slot].id==gesture_tracking&&e->value<0)gesture_ended=true;
                if(e->value>=0) {
                    if(input_owner==INPUT_PANEL&&gesture_ended)replaced=true;
                    ++mt_starts;
                    if(first_new<0){first_new=(int)current_slot;first_id=e->value;}
                    if(before>0&&input_owner==INPUT_PANEL
                       &&((int)current_slot!=gesture_slot||e->value!=gesture_tracking))replaced=true;
                    began=true;
                }
                contacts[current_slot].id=e->value;
            } else if(e->code==ABS_MT_POSITION_X)contacts[current_slot].x=e->value;
            else if(e->code==ABS_MT_POSITION_Y)contacts[current_slot].y=e->value;
            else if(e->code==ABS_X)raw_x=e->value;
            else if(e->code==ABS_Y)raw_y=e->value;
        } else if(e->type==EV_KEY&&(e->code==BTN_TOUCH||e->code==BTN_LEFT)) {
            if(e->value==0)key_lifted=true;
            if(e->value==1){began=true;if(before>0&&key_lifted)replaced=true;}
            if(e->code==BTN_TOUCH)key_touch=e->value!=0;else key_left=e->value!=0;
        }
        unsigned n=contact_count();if(n>maximum)maximum=n;
    }
    unsigned after=contact_count();
    uint64_t desired=atomic_load_explicit(&wanted_view,memory_order_acquire);
    if(atomic_load_explicit(&presented_view,memory_order_acquire)!=desired
       ||atomic_load_explicit(&acknowledged_epoch,memory_order_acquire)!=atomic_load_explicit(&submitted_epoch,memory_order_acquire))
        input_owner=INPUT_DRAIN;
    int slot=first_new;
    if(slot<0)for(unsigned i=0;i<32;++i)if(contacts[i].id>=0){slot=(int)i;break;}
    if(input_owner==INPUT_NONE&&before==0&&(after>0||began)) {
        gesture_slot=slot;gesture_tracking=first_id;gesture_ended=false;
        int rx=slot>=0?contacts[slot].x:raw_x,ry=slot>=0?contacts[slot].y:raw_y;
        if(rx<0||rx>RAW_TOUCH_HEIGHT||ry<0||ry>(int)WIDTH) {
            input_owner=INPUT_DRAIN;fault("touch coordinates outside qualified geometry");return;
        }
        int x=ry,y=RAW_TOUCH_HEIGHT-rx;
        input_owner=(desired&1)||inside(x,y,600,14,150,52)?INPUT_PANEL:INPUT_STOCK;
        contact_pressed=true;gesture_action=ACTION_NONE;
        if(input_owner==INPUT_PANEL)gesture_action=hit(&displayed_scene,x,y,&gesture_command);
    }
    if(input_owner==INPUT_PANEL&&(maximum>1||mt_starts>1||replaced))gesture_action=ACTION_NONE;
    bool pass=input_owner==INPUT_STOCK||(input_owner==INPUT_NONE&&!(desired&1));
    if(after==0&&(before>0||began)) {
        if(input_owner==INPUT_PANEL&&contact_pressed) {
            int rx=gesture_slot>=0?contacts[gesture_slot].x:raw_x;
            int ry=gesture_slot>=0?contacts[gesture_slot].y:raw_y;
            if(rx>=0&&rx<=RAW_TOUCH_HEIGHT&&ry>=0&&ry<=(int)WIDTH)finish_gesture(ry,RAW_TOUCH_HEIGHT-rx);
        }
        contact_pressed=false;gesture_action=ACTION_NONE;
        if(input_owner!=INPUT_DRAIN)input_owner=INPUT_NONE;
    }
    if(pass) {
        memcpy(replay,event_frame,frame_count*sizeof *replay);
        replay_count=frame_count;replay_cursor=0;
    }
}
static NOINLINE ssize_t ui_read(int fd,void *buffer,size_t size) {
    uintptr_t site=call_site(__builtin_return_address(0));
    if(!is_gui()||(site!=SITE_READ1&&site!=SITE_READ2))return vendor_read(fd,buffer,size);
    uint64_t before=atomic_load_explicit(&acknowledged_epoch,memory_order_acquire);
    pthread_mutex_lock(&input_mutex);
    consume_completions();
    if(atomic_load_explicit(&failed,memory_order_acquire)||atomic_load_explicit(&retiring_mapping,memory_order_acquire)) {
        input_owner=INPUT_DRAIN;gesture_action=ACTION_NONE;
        replay_cursor=0;replay_count=0;frame_count=0;
    }
    if(size!=sizeof(struct input_event)) {
        fault("unexpected touch read size");pthread_mutex_unlock(&input_mutex);restore_vendor_errno(EIO);return -1;
    }
    if(replay_cursor<replay_count) {
        memcpy(buffer,&replay[replay_cursor++],sizeof(struct input_event));
        pthread_mutex_unlock(&input_mutex);return sizeof(struct input_event);
    }
    ssize_t n=vendor_read(fd,buffer,size);int saved=saved_vendor_errno();
    bool identity=touchscreen(fd);
    if(!identity){input_owner=INPUT_DRAIN;fault("touchscreen identity changed");}
    if(n<0&&(saved==EAGAIN||saved==EWOULDBLOCK)&&identity)release_barrier(fd,before);
    if(n>0) {
        if(n!=(ssize_t)sizeof(struct input_event)) {
            fault("short touchscreen record");pthread_mutex_unlock(&input_mutex);restore_vendor_errno(EIO);return -1;
        }
        struct input_event *e=buffer;
        if(e->type==EV_SYN&&e->code==SYN_DROPPED) {
            input_owner=INPUT_DRAIN;frame_count=0;fault("touch stream dropped; retiring UI");
        } else if(identity) {
            if(frame_count==256){input_owner=INPUT_DRAIN;frame_count=0;fault("touch report exceeds bound");}
            else {
                event_frame[frame_count++]=*e;
                if(e->type==EV_SYN&&e->code==SYN_REPORT){process_frame();frame_count=0;}
            }
        }
        // Neutral placeholders keep stock's existing record-drain loop moving.
        // Complete stock-owned reports are replayed, byte-for-byte, before any
        // more kernel records; panel gestures never produce a stock prefix.
        e->type=EV_SYN;e->code=SYN_REPORT;e->value=0;
    }
    pthread_mutex_unlock(&input_mutex);restore_vendor_errno(saved);return n;
}
static NOINLINE int ui_select(int nfds,fd_set *reads,fd_set *writes,fd_set *exceptions,struct timeval *wait) {
    uintptr_t site=call_site(__builtin_return_address(0));
    uint64_t before=atomic_load_explicit(&acknowledged_epoch,memory_order_acquire);
    int input=-1;bool draining=false;
    if(is_gui()&&site==SITE_SELECT&&reads) {
        for(int fd=0;fd<nfds&&fd<FD_SETSIZE;++fd)if(FD_ISSET(fd,reads)){input=fd;break;}
        pthread_mutex_lock(&input_mutex);consume_completions();
        bool ready=replay_cursor<replay_count;
        draining=input_owner==INPUT_DRAIN;
        pthread_mutex_unlock(&input_mutex);
        if(ready&&input>=0) {
            FD_ZERO(reads);FD_SET(input,reads);if(writes)FD_ZERO(writes);if(exceptions)FD_ZERO(exceptions);
            return 1;
        }
    }
    // A post-flip empty-queue barrier should not cost the stock 500 ms
    // polling interval (or swallow the user's first launcher tap at startup).
    struct timeval short_wait={.tv_usec=50000};
    struct timeval *effective=wait;
    if(draining&&wait&&(wait->tv_sec>0||wait->tv_usec>50000))effective=&short_wait;
    int result=vendor_select(nfds,reads,writes,exceptions,effective);int saved=saved_vendor_errno();
    if(effective!=wait)*wait=short_wait;
    if(result==0&&input>=0) {
        pthread_mutex_lock(&input_mutex);consume_completions();
        if(touchscreen(input))release_barrier(input,before);
        pthread_mutex_unlock(&input_mutex);
    }
    restore_vendor_errno(saved);return result;
}
static void consume_completions(void) {
    for(unsigned i=0;i<4;++i) {
        unsigned state=TICKET_COMPLETE;
        if(!atomic_compare_exchange_strong_explicit(&tickets[i].state,&state,TICKET_READING,memory_order_acquire,memory_order_relaxed))continue;
        if(tickets[i].scene.serial>displayed_scene.serial) {
            bool changed=tickets[i].scene.epoch!=displayed_scene.epoch;
            displayed_scene=tickets[i].scene;
            atomic_store_explicit(&presented_view,displayed_scene.view,memory_order_release);
            atomic_store_explicit(&acknowledged_epoch,displayed_scene.epoch,memory_order_release);
            if(changed){input_owner=INPUT_DRAIN;gesture_action=ACTION_NONE;}
        }
        atomic_store_explicit(&tickets[i].state,TICKET_FREE,memory_order_release);
    }
}
static struct FlipTicket *find_ticket(void *data) {
    for(unsigned i=0;i<4;++i)if(data==&tickets[i]&&atomic_load_explicit(&tickets[i].state,memory_order_acquire)==TICKET_QUEUED)return &tickets[i];
    return NULL;
}
static void acknowledge(struct FlipTicket *ticket) {
    if(!atomic_exchange_explicit(&first_presented,true,memory_order_acq_rel)&&is_gui()) {
        mark_session("frame-ready");note("first RC2FX launcher scene physically presented");
    }
    atomic_store_explicit(&ticket->state,TICKET_COMPLETE,memory_order_release);
    // Never discard a contended completion. The next read/select/commit will
    // consume the retained scene; the callback itself never waits on input.
    if(pthread_mutex_trylock(&input_mutex)==0){consume_completions();pthread_mutex_unlock(&input_mutex);}
}
static void page_flip(int fd,unsigned sequence,unsigned sec,unsigned usec,void *data) {
    struct FlipTicket *ticket=find_ticket(data);if(ticket)acknowledge(ticket);
    if(previous_flip)previous_flip(fd,sequence,sec,usec,ticket?NULL:data);
}
static void page_flip2(int fd,unsigned sequence,unsigned sec,unsigned usec,unsigned crtc,void *data) {
    struct FlipTicket *ticket=find_ticket(data);if(ticket)acknowledge(ticket);
    if(previous_flip2)previous_flip2(fd,sequence,sec,usec,crtc,ticket?NULL:data);
    else if(previous_flip)previous_flip(fd,sequence,sec,usec,ticket?NULL:data);
}
static bool same_bindings(const struct UiScene *a,const struct UiScene *b) {
    if(a->view!=b->view)return false;
    if(!(a->view&1))return true; // Hidden panel data cannot change launcher hitboxes.
    if(a->page!=b->page||strcmp(a->selected,b->selected)||a->state.count!=b->state.count
       ||a->state.revision!=b->state.revision||a->state.generation!=b->state.generation
       ||a->state.connected!=b->state.connected||a->state.ready!=b->state.ready||a->state.can_enable!=b->state.can_enable
       ||a->state.can_disable!=b->state.can_disable||a->state.busy!=b->state.busy)return false;
    for(unsigned i=0;i<a->state.count;++i)if(strcmp(a->state.presets[i].id,b->state.presets[i].id))return false;
    return true;
}
static NOINLINE int ui_commit(int fd,void *request,uint32_t flags,void *data) {
    uintptr_t site=call_site(__builtin_return_address(0));
    struct FlipTicket *ticket=NULL;
    static struct UiScene previous;static bool have_previous;
    if(is_gui()&&site==SITE_COMMIT&&!atomic_load_explicit(&failed,memory_order_acquire)) {
        if(pthread_mutex_trylock(&input_mutex)==0){consume_completions();pthread_mutex_unlock(&input_mutex);}
        if(!conversion_ready||data!=NULL||(flags!=1&&flags!=0x401))fault("unexpected presentation path");
        else {
            for(unsigned i=0;i<4;++i)if(atomic_load_explicit(&tickets[i].state,memory_order_acquire)==TICKET_FREE){ticket=&tickets[i];break;}
            if(ticket) {
                drawing_scene.epoch=!have_previous||!same_bindings(&drawing_scene,&previous)?++next_epoch:previous.epoch;
                drawing_scene.serial=++next_serial;
                previous=drawing_scene;have_previous=true;ticket->scene=drawing_scene;
                atomic_store_explicit(&ticket->state,TICKET_QUEUED,memory_order_release);
                // Quarantine before the physical change, including paging and
                // reordered preset IDs, not only modal open/close transitions.
                atomic_store_explicit(&submitted_epoch,drawing_scene.epoch,memory_order_release);
            } else fault("unretired DRM page-flip tickets");
        }
        conversion_ready=false;
    }
    int result=vendor_commit(fd,request,flags,ticket?(void *)ticket:data);int saved=saved_vendor_errno();
    if(result!=0&&ticket){atomic_store_explicit(&ticket->state,TICKET_FREE,memory_order_release);fault("DRM commit failed");}
    restore_vendor_errno(saved);return result;
}
static NOINLINE int ui_handle_event(int fd,drmEventContextPtr context) {
    uintptr_t site=call_site(__builtin_return_address(0));
    if(!is_gui()||site!=SITE_HANDLE)return vendor_handle_event(fd,context);
    if(!context||context->version<2||context->version>4) {
        fault("unsupported DRM callback context");return vendor_handle_event(fd,context);
    }
    // Copy only fields belonging to the advertised version: older vendor
    // contexts need not allocate the tail of today's public C struct.
    drmEventContext wrapped={.version=context->version};
    wrapped.vblank_handler=context->vblank_handler;
    previous_flip=context->page_flip_handler;wrapped.page_flip_handler=page_flip;
    if(context->version>=3){previous_flip2=context->page_flip_handler2;
        wrapped.page_flip_handler2=previous_flip2?page_flip2:NULL;}
    if(context->version>=4)wrapped.sequence_handler=context->sequence_handler;
    int result=vendor_handle_event(fd,&wrapped);int saved=saved_vendor_errno();
    previous_flip=NULL;previous_flip2=NULL;
    if(result!=0)fault("DRM event delivery failed");
    restore_vendor_errno(saved);return result;
}
/* The pinned frame loop waits on its native event BEFORE draining the prior
 * DRM flip. Stock animation is not a repaint guarantee. Wake that SAME native
 * event at a bounded rate, without constructing or patching any JUCE object.
 * Event wait/signal bodies and +8 mutex/+0x38 condition are byte-mapped for the
 * admitted GUI (signal 0x8bf130, wait 0x930100); the whole executable is hashed.
 */
static void *frame_pump(void *unused) {
    (void)unused;
    cpu_set_t cpus;CPU_ZERO(&cpus);CPU_SET(0,&cpus);
    (void)pthread_setaffinity_np(pthread_self(),sizeof cpus,&cpus);
    (void)setpriority(PRIO_PROCESS,(id_t)syscall(SYS_gettid),10);
    for(;;) {
        uint64_t desired=atomic_load_explicit(&wanted_view,memory_order_acquire);
        bool fast=(desired&1)||desired!=atomic_load_explicit(&presented_view,memory_order_acquire)
            ||atomic_load_explicit(&submitted_epoch,memory_order_acquire)!=atomic_load_explicit(&acknowledged_epoch,memory_order_acquire);
        struct timespec pause={.tv_nsec=fast?100000000:250000000};
        nanosleep(&pause,NULL);
        pthread_mutex_lock(&event_mutex);
        if(renderer_event&&atomic_load_explicit(&shadow_live,memory_order_acquire)
           &&!atomic_load_explicit(&failed,memory_order_acquire))signal_renderer(renderer_event);
        pthread_mutex_unlock(&event_mutex);
    }
    return NULL;
}
static NOINLINE void ui_condition_wait(void *condition,void *lock) {
    uintptr_t site=call_site(__builtin_return_address(0));
    if(is_gui()&&site==0x93015c&&syscall(SYS_gettid)==atomic_load_explicit(&renderer_tid,memory_order_acquire)
       &&condition&&lock&&atomic_load_explicit(&shadow_live,memory_order_acquire)) {
        void *event=(char *)condition-0x38;
        void *native_mutex;memcpy(&native_mutex,lock,sizeof native_mutex);
        if(native_mutex!=(char *)event+8)fault("renderer event ABI mismatch");
        else if(pthread_mutex_trylock(&event_mutex)==0) {
            // The caller still owns the native event mutex. Never block on
            // our retirement lock here: the pump may own it while waiting
            // for that native mutex. Entering the original wait releases it.
            renderer_event=event;
            if(!pump_started) {
                pthread_t thread;
                if(pthread_create(&thread,NULL,frame_pump,NULL)==0){pthread_detach(thread);pump_started=true;}
                else fault("renderer wake thread unavailable");
            }
            pthread_mutex_unlock(&event_mutex);
        } else if(!pump_started)fault("initial renderer discovery contended; retiring rather than leaving an unwakeable UI");
    }
    vendor_condition_wait(condition,lock);
}
static NOINLINE void ui_condition_destroy(void *condition) {
    if(is_gui()) {
        // Serialize retirement with any in-flight native signal. The pump
        // can never keep a pointer to an event after its condition destructor.
        pthread_mutex_lock(&event_mutex);
        if(renderer_event&&(char *)renderer_event+0x38==(char *)condition)renderer_event=NULL;
        pthread_mutex_unlock(&event_mutex);
    }
    vendor_condition_destroy(condition);
}
static NOINLINE int ui_munmap(void *address,size_t size) {
    if(is_gui()&&paint_pixels&&(address==paint_pixels||address==driver_pixels)) {
        if(size!=source_length){fault("unexpected source unmap length");restore_vendor_errno(EINVAL);return -1;}
        /* Keep the hidden real mapping until process exit, even when queued.
         * Returning success for the vendor's shadow unmap cannot free DMA. */
        atomic_store_explicit(&shadow_live,false,memory_order_release);
        pthread_mutex_lock(&event_mutex);renderer_event=NULL;pthread_mutex_unlock(&event_mutex);
        retiring_mapping=true;
        ui_control_revoke();
        /* Initialization/reconfiguration is outside this preview. Do not
         * allow a later map/QBUF to reuse a retired shadow or driver mapping. */
        request_exit("video source retired; returning to stock GUI");
        void *shadow=paint_pixels;paint_pixels=NULL;
        return vendor_munmap(shadow,size);
    }
    return vendor_munmap(address,size);
}
static NOINLINE int ui_close(int fd) {
    // Forked vendor helpers inherit this module before exec, but not its
    // threads. Never touch copied mutexes or signal the parent's RAM session.
    if(!is_gui())return vendor_close(fd);
    if(fd==video_fd) {
        if(atomic_load_explicit(&shadow_live,memory_order_acquire))
            fault("video device closed; retiring preview");
        video_fd=-1;
    }
    if(fd==touch_fd){
        pthread_mutex_lock(&input_mutex);
        touch_fd=-1;contact_pressed=false;have_x=false;have_y=false;
        replay_cursor=0;replay_count=0;frame_count=0;input_owner=INPUT_DRAIN;
        pthread_mutex_unlock(&input_mutex);
    }

    return vendor_close(fd);
}

EXPORT unsigned int la_version(unsigned int version) {return version>=LAV_CURRENT?LAV_CURRENT:0;}
EXPORT unsigned int la_objopen(struct link_map *map,Lmid_t namespace,uintptr_t *cookie) {
    if(namespace!=LM_ID_BASE)return 0;
    bool main=map->l_name[0]=='\0';
    if(main){main_base=(uintptr_t)map->l_addr;main_seen=true;}
    *cookie=main?1:2;
    return LA_FLG_BINDTO|LA_FLG_BINDFROM;
}
EXPORT void la_preinit(uintptr_t *cookie) {
    (void)cookie;
    const char *opt=getenv("RC2FX_UI_PREVIEW");
    close_path=getenv("RC2FX_UI_CLOSE");
    if(!opt||strcmp(opt,"1")!=0||!close_path||strncmp(close_path,"/run/rc2fx-ui.",14)!=0)
        return;
    if(!main_seen||!admitted_executable()){note("GUI hash not qualified; no hooks active");return;}
    void *base_libc=dlmopen(LM_ID_BASE,"libc.so.6",RTLD_NOW|RTLD_NOLOAD);
    void *errno_symbol=base_libc?dlsym(base_libc,"__errno_location"):NULL;
    if(!errno_symbol){note("base libc errno unavailable; no hooks active");return;}
    _Static_assert(sizeof vendor_errno==sizeof errno_symbol,"ELF function pointer ABI");
    memcpy(&vendor_errno,&errno_symbol,sizeof vendor_errno);
    /* Keep this existing-libc reference for the lifetime of the preview. */
    _Static_assert(sizeof(struct input_event)==24,"pinned input_event ABI");
    _Static_assert(sizeof(struct v4l2_buffer)==88,"pinned V4L2 ABI");
    _Static_assert(VIDIOC_QBUF==0xc058560fUL,"pinned ioctl ABI");
    for(unsigned i=0;i<32;++i)contacts[i].id=-1;
    uintptr_t signal_address=main_base+0x8bf130;
    memcpy(&signal_renderer,&signal_address,sizeof signal_renderer);
    gui_pid=getpid();enabled=true;
    if(!ui_control_start("/run/rc2fx/ui.sock"))note("controller worker unavailable; panel will remain read-only");
    note("admitted exact Pro II 1.7.7 GUI; RC2FX launcher active");
}
EXPORT uintptr_t la_symbind64(Elf64_Sym *symbol,unsigned int index,uintptr_t *reference,
                              uintptr_t *definition,unsigned int *flags,const char *name) {
    (void)index;(void)definition;
    uintptr_t original=(uintptr_t)symbol->st_value;
    if(*reference!=1)return original;
    /* These wrappers use the imported C ABI only. No direct JUCE branches or
     * vendor addresses are modified. Each real function is loader-supplied. */
#define BIND(symbol_name,storage,replacement) \
    if(strcmp(name,symbol_name)==0){ \
        _Static_assert(sizeof storage==sizeof original,"ELF function pointer ABI"); \
        memcpy(&storage,&original,sizeof storage); \
        *flags|=LA_SYMB_NOPLTENTER|LA_SYMB_NOPLTEXIT; \
        return (uintptr_t)&replacement; \
    }
    BIND("ioctl",vendor_ioctl,ui_ioctl)
    BIND("mmap",vendor_mmap,ui_mmap)
    BIND("munmap",vendor_munmap,ui_munmap)
    BIND("close",vendor_close,ui_close)
    BIND("read",vendor_read,ui_read)
    BIND("select",vendor_select,ui_select)
    BIND("drmModeAtomicCommit",vendor_commit,ui_commit)
    BIND("drmHandleEvent",vendor_handle_event,ui_handle_event)
    BIND("_ZNSt18condition_variable4waitERSt11unique_lockISt5mutexE",vendor_condition_wait,ui_condition_wait)
    BIND("_ZNSt18condition_variableD1Ev",vendor_condition_destroy,ui_condition_destroy)
#undef BIND
    return original;
}
