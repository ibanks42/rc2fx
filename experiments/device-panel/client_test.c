/* Run only against the parent test's local, runtime-less Rust controller. */
#include "control.c"
#include <inttypes.h>
#include <stdlib.h>

#define CHECK(condition) do { \
    if(!(condition)) { \
        fprintf(stderr,"client_test:%d: assertion failed: %s\n",__LINE__,#condition); \
        exit(EXIT_FAILURE); \
    } \
} while(0)

static void expect_status(unsigned actual,unsigned expected) {
    if(actual!=expected) {
        fprintf(stderr,"client_test: assertion failed: expected status %u, got %u\n",
                expected,actual);
        exit(EXIT_FAILURE);
    }
}

int main(int argc,char **argv) {
    if(argc!=2) {
        fprintf(stderr,"usage: %s /absolute/path/to/test-ui.sock\n",argv[0]);
        return EXIT_FAILURE;
    }
    CHECK(argv[1][0]=='/'&&strlen(argv[1])<sizeof socket_name);
    memcpy(socket_name,argv[1],strlen(argv[1])+1);
    int fd=connect_controller();
    if(fd<0) {
        fprintf(stderr,"client_test: assertion failed: connect %s: %s\n",
                socket_name,strerror(errno));
        return EXIT_FAILURE;
    }

    const struct UiCommand snapshot={0};
    struct UiSnapshot initial,reply;
    unsigned status=0;
    CHECK(rpc(fd,&snapshot,&initial,&status));
    expect_status(status,200);
    CHECK(initial.connected&&initial.count==1);
    CHECK(strcmp(initial.presets[0].name,"VOICE")==0);
    CHECK(initial.presets[0].id[0]!='\0'&&initial.revision<UINT64_MAX);

    /* Do not gate on ready: this fixture deliberately has no audio runtime.
     * Pass the real generation to the C serializer; apply must zero it on the
     * wire, as required by Rust's actual decoder (unlike processing toggles). */
    struct UiCommand apply={
        .op=1,.revision=initial.revision,.generation=initial.generation
    };
    memcpy(apply.id,initial.presets[0].id,sizeof apply.id);
    const struct UiCommand original=apply;
    CHECK(rpc(fd,&apply,&reply,&status));
    expect_status(status,200);
    CHECK(reply.connected&&reply.revision==initial.revision+1);
    CHECK(reply.count==1&&strcmp(reply.presets[0].id,apply.id)==0);
    CHECK(apply.revision==original.revision&&apply.generation==original.generation);
    CHECK(apply.op==original.op&&apply.enabled==original.enabled);
    CHECK(strcmp(apply.id,original.id)==0);

    CHECK(rpc(fd,&apply,&reply,&status)); /* The exact same, now stale request. */
    expect_status(status,409);
    CHECK(rpc(fd,&snapshot,&reply,&status));
    expect_status(status,200);
    CHECK(reply.connected&&reply.revision==initial.revision+1);
    CHECK(reply.count==1&&strcmp(reply.presets[0].id,initial.presets[0].id)==0);
    CHECK(strcmp(reply.presets[0].name,"VOICE")==0);
    CHECK(close(fd)==0);
    printf("client_test: PASS snapshot, apply, stale 409; revision %" PRIu64 " -> %" PRIu64 "\n",
           initial.revision,reply.revision);
    return EXIT_SUCCESS;
}
