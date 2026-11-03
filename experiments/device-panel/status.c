/* Read-only probe of the ACTUAL local touchscreen controller endpoint.
 * Shares the UI client's real framing and decoder; never submits a mutation. */
#include "control.c"
#include <inttypes.h>
#include <stdlib.h>
int main(int argc,char **argv) {
    const char *path=argc==2?argv[1]:"/run/rc2fx/ui.sock";
    if(argc>2||path[0]!='/'||strlen(path)>=sizeof socket_name)return 2;
    memcpy(socket_name,path,strlen(path)+1);
    int fd=connect_controller();
    if(fd<0){fprintf(stderr,"Touchscreen controller unavailable: %s\n",strerror(errno));return 1;}
    struct UiCommand request={0};struct UiSnapshot state;unsigned status;
    bool ok=rpc(fd,&request,&state,&status);close(fd);
    if(!ok){fputs("Invalid or timed-out touchscreen reply\n",stderr);return 1;}
    if(status!=200){fprintf(stderr,"Controller status %u: %s\n",status,state.message);return 1;}
    printf("Revision: %" PRIu64 "  Generation: %" PRIu64 "\n",state.revision,state.generation);
    printf("Processing: %s  Applied: %s  Presets ready: %s\n",state.known?(state.enabled?"ON":"OFF"):"PENDING",state.applied?"yes":"no",state.ready?"yes":"no");
    printf("Can turn on: %s  Can turn off: %s\n",state.can_enable?"yes":"no",state.can_disable?"yes":"no");
    printf("Presets: %u\n",state.count);
    for(unsigned i=0;i<state.count;++i)printf("  %s  %s\n",state.presets[i].id,state.presets[i].name);
    if(state.message[0])printf("Status: %s\n",state.message);
    return 0;
}
