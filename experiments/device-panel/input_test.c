/* Host-only tests of the actual implementation. No loader entry points,
 * touch_quiet/release_barrier, device I/O or native event pump run. One close
 * wrapper is tested against a local pipe to verify forked-helper isolation.
 * INPUT_NONE fixtures assume an already-established empty-queue barrier;
 * these tests do not validate that hardware-dependent barrier itself. */
#include "rc2fx_ui.c"
#include <sys/wait.h>
#include <signal.h>

#define CHECK(condition) do { \
    if(!(condition)) { \
        fprintf(stderr,"input_test:%d: assertion failed: %s\n",__LINE__,#condition); \
        exit(EXIT_FAILURE); \
    } \
} while(0)
#define COUNT(a) (sizeof(a)/sizeof((a)[0]))

struct Report { struct input_event events[32]; size_t count; };

static void add(struct Report *r,unsigned short type,unsigned short code,int value) {
    CHECK(r->count<COUNT(r->events));
    r->events[r->count]=(struct input_event){
        .time={.tv_sec=12345,.tv_usec=(long)(r->count*137+1)},
        .type=type,.code=code,.value=value
    };
    ++r->count;
}

static void send_report(struct Report *r,bool stock) {
    add(r,EV_SYN,SYN_REPORT,0);
    CHECK(frame_count==0&&replay_cursor==replay_count);
    size_t old_count=replay_count,old_cursor=replay_cursor;
    struct input_event old_replay[COUNT(replay)];
    memcpy(old_replay,replay,sizeof replay);
    memcpy(event_frame,r->events,r->count*sizeof *event_frame);
    frame_count=r->count;
    process_frame();
    frame_count=0; /* As in ui_read, without calling that syscall hook. */
    CHECK(!atomic_load(&failed)&&!atomic_load(&close_requested));
    if(stock) {
        CHECK(replay_count==r->count&&replay_cursor==0);
        CHECK(memcmp(replay,r->events,r->count*sizeof *replay)==0);
        replay_cursor=replay_count; /* All verified replay records consumed. */
    } else {
        CHECK(replay_count==old_count&&replay_cursor==old_cursor);
        CHECK(memcmp(replay,old_replay,sizeof replay)==0);
    }
    CHECK(memcmp(event_frame,r->events,r->count*sizeof *event_frame)==0);
}

static void reset_input(bool open,unsigned count) {
    CHECK(count<=UI_MAX_PRESETS);
    atomic_store(&failed,false);
    atomic_store(&close_requested,false);
    atomic_store(&shadow_live,false);
    close_path=NULL;
    enabled=false;
    touch_fd=-1;
    touch_device=0;touch_inode=0;
    raw_x=120;raw_y=100;have_x=false;have_y=false;
    current_slot=0;
    for(unsigned i=0;i<COUNT(contacts);++i)
        contacts[i]=(struct Contact){.id=-1,.x=raw_x,.y=raw_y};
    key_touch=false;key_left=false;contact_pressed=false;
    gesture_ended=false;gesture_slot=-1;gesture_tracking=-1;
    atomic_store(&retiring_mapping,false);
    input_owner=INPUT_NONE;
    gesture_action=ACTION_NONE;
    memset(&gesture_command,0,sizeof gesture_command);
    memset(selected_id,0,sizeof selected_id);
    page=0;
    memset(event_frame,0,sizeof event_frame);
    memset(replay,0,sizeof replay);
    frame_count=0;replay_count=0;replay_cursor=0;
    memset(&displayed_scene,0,sizeof displayed_scene);
    displayed_scene.view=open?1:0;
    displayed_scene.epoch=10;displayed_scene.serial=10;
    displayed_scene.state=(struct UiSnapshot){
        .connected=true,.known=true,.enabled=true,.ready=true,.applied=true,
        .can_disable=true,.revision=42,.generation=7,.count=count
    };
    for(unsigned i=0;i<count;++i) {
        snprintf(displayed_scene.state.presets[i].id,
                 sizeof displayed_scene.state.presets[i].id,"preset-%u",i);
        snprintf(displayed_scene.state.presets[i].name,
                 sizeof displayed_scene.state.presets[i].name,"PRESET %u",i);
    }
    drawing_scene=displayed_scene;
    source_view=displayed_scene.view;
    atomic_store(&wanted_view,displayed_scene.view);
    atomic_store(&presented_view,displayed_scene.view);
    atomic_store(&submitted_epoch,displayed_scene.epoch);
    atomic_store(&acknowledged_epoch,displayed_scene.epoch);
    next_epoch=displayed_scene.epoch;next_serial=displayed_scene.serial;
    for(unsigned i=0;i<COUNT(tickets);++i) {
        memset(&tickets[i].scene,0,sizeof tickets[i].scene);
        atomic_store(&tickets[i].state,TICKET_FREE);
    }
    previous_flip=NULL;previous_flip2=NULL;
    ui_control_visible(false); /* The real control worker is never started. */
    /* process_frame's local gesture_slot is assigned by every new down. */
}

static struct Report mt_down(unsigned slot,int id,int x,int y,unsigned key,bool key_first) {
    struct Report r={0};
    if(key&&key_first)add(&r,EV_KEY,(unsigned short)key,1);
    add(&r,EV_ABS,ABS_MT_SLOT,(int)slot);
    add(&r,EV_ABS,ABS_MT_TRACKING_ID,id);
    add(&r,EV_ABS,ABS_MT_POSITION_X,RAW_TOUCH_HEIGHT-y);
    add(&r,EV_ABS,ABS_MT_POSITION_Y,x);
    if(key&&!key_first)add(&r,EV_KEY,(unsigned short)key,1);
    return r;
}

static struct Report mt_up(unsigned slot,unsigned key) {
    struct Report r={0};
    add(&r,EV_ABS,ABS_MT_SLOT,(int)slot);
    add(&r,EV_ABS,ABS_MT_TRACKING_ID,-1);
    if(key)add(&r,EV_KEY,(unsigned short)key,0);
    return r;
}

static void tap(int x,int y) {
    struct Report down=mt_down(0,11,x,y,BTN_TOUCH,true);
    send_report(&down,false);
    struct Report up=mt_up(0,BTN_TOUCH);
    send_report(&up,false);
}

static void test_launcher_order(void) {
    for(unsigned key_first=0;key_first<2;++key_first) {
        reset_input(false,0);
        struct Report down=mt_down(0,11,650,40,BTN_TOUCH,key_first!=0);
        send_report(&down,false);
        CHECK(input_owner==INPUT_PANEL&&gesture_action==ACTION_LAUNCH);
        CHECK(atomic_load(&wanted_view)==0);
        struct Report up=mt_up(0,BTN_TOUCH);
        send_report(&up,false);
        CHECK(atomic_load(&wanted_view)==3);
        CHECK(atomic_load(&presented_view)==0&&input_owner==INPUT_DRAIN);
        CHECK(contact_count()==0&&!contact_pressed&&gesture_action==ACTION_NONE);
    }
    puts("input_test: PASS KEY-first and MT-first launcher taps");
}

static void test_queued_coordinates_and_stock_replay(void) {
    reset_input(false,0);
    /* Misleading retained single-touch and other-slot coordinates cannot
     * classify this complete, queued slot-3 report. No kernel query is used. */
    raw_x=20;raw_y=1200;
    contacts[0].x=20;contacts[0].y=1200;
    struct Report down=mt_down(3,21,650,40,BTN_TOUCH,true);
    send_report(&down,false);
    CHECK(input_owner==INPUT_PANEL&&gesture_action==ACTION_LAUNCH);
    struct Report up=mt_up(3,BTN_TOUCH);
    send_report(&up,false);
    CHECK(atomic_load(&wanted_view)==3);

    reset_input(false,0);
    raw_x=680;raw_y=650;
    contacts[0].x=680;contacts[0].y=650;
    /* The converse: stale launcher coordinates must not steal stock input.
     * Nonzero timestamps and ignored fields must survive byte-for-byte. */
    down=mt_down(0,22,100,400,BTN_TOUCH,true);
    add(&down,EV_ABS,ABS_MT_PRESSURE,37);
    send_report(&down,true);
    CHECK(input_owner==INPUT_STOCK&&gesture_action==ACTION_NONE);
    struct Report move={0};
    add(&move,EV_ABS,ABS_MT_POSITION_X,310);
    add(&move,EV_ABS,ABS_MT_POSITION_Y,120);
    send_report(&move,true);
    up=mt_up(0,BTN_TOUCH);
    send_report(&up,true);
    CHECK(input_owner==INPUT_NONE&&contact_count()==0);
    CHECK(atomic_load(&wanted_view)==0);
    /* A following UI gesture must neither replay nor alter the exhausted
     * stock replay buffer, even when it still contains a previous report. */
    tap(650,40);
    CHECK(atomic_load(&wanted_view)==3&&input_owner==INPUT_DRAIN);
    puts("input_test: PASS queued coordinates and byte-exact stock replay");
}

static void test_btn_left(void) {
    for(unsigned mt=0;mt<2;++mt) {
        reset_input(false,0);
        struct Report down={0};
        if(mt)down=mt_down(0,31,650,40,BTN_LEFT,true);
        else {
            add(&down,EV_KEY,BTN_LEFT,1);
            add(&down,EV_ABS,ABS_X,680);
            add(&down,EV_ABS,ABS_Y,650);
        }
        send_report(&down,false);
        CHECK(key_left&&!key_touch&&gesture_action==ACTION_LAUNCH);
        struct Report up={0};
        if(mt)up=mt_up(0,BTN_LEFT);
        else add(&up,EV_KEY,BTN_LEFT,0);
        send_report(&up,false);
        CHECK(!key_left&&contact_count()==0&&atomic_load(&wanted_view)==3);
        CHECK(input_owner==INPUT_DRAIN);
    }
    puts("input_test: PASS matching BTN_LEFT releases (single-touch and MT)");
}

static void test_multiple_contacts(void) {
    for(unsigned transient=0;transient<2;++transient) {
        reset_input(false,0);
        struct Report down=mt_down(0,41,650,40,BTN_TOUCH,true);
        send_report(&down,false);
        CHECK(gesture_action==ACTION_LAUNCH);
        struct Report second=mt_down(1,42,650,40,0,false);
        if(transient)add(&second,EV_ABS,ABS_MT_TRACKING_ID,-1);
        send_report(&second,false);
        CHECK(input_owner==INPUT_PANEL&&gesture_action==ACTION_NONE);
        if(!transient) {
            struct Report second_up=mt_up(1,0);
            send_report(&second_up,false);
        }
        struct Report up=mt_up(0,BTN_TOUCH);
        send_report(&up,false);
        CHECK(contact_count()==0&&atomic_load(&wanted_view)==0);
    }
    puts("input_test: PASS multiple contacts cancel, including a transient second contact");
}

static void queue_scene(unsigned index,const struct UiScene *scene) {
    CHECK(index<COUNT(tickets)&&atomic_load(&tickets[index].state)==TICKET_FREE);
    tickets[index].scene=*scene;
    atomic_store(&tickets[index].state,TICKET_QUEUED);
}

static void test_modal_close(void) {
    reset_input(true,1);
    tap(100,400); /* Even outside the modal, UI-owned reports never replay. */
    CHECK(atomic_load(&wanted_view)==1&&input_owner==INPUT_NONE);
    tap(900,575);
    CHECK(atomic_load(&wanted_view)==2&&atomic_load(&presented_view)==1);
    CHECK(input_owner==INPUT_DRAIN);
    tap(100,400); /* No stock ownership while the close is not presented. */
    CHECK(input_owner==INPUT_DRAIN&&atomic_load(&wanted_view)==2);
    struct UiScene closed=displayed_scene;
    closed.view=2;closed.epoch=11;closed.serial=11;
    queue_scene(0,&closed);
    atomic_store(&submitted_epoch,closed.epoch);
    page_flip(-1,1,0,0,&tickets[0]);
    CHECK(atomic_load(&presented_view)==2&&atomic_load(&acknowledged_epoch)==11);
    CHECK(input_owner==INPUT_DRAIN&&atomic_load(&tickets[0].state)==TICKET_FREE);
    tap(650,40); /* ACK alone does not establish an empty-queue barrier. */
    CHECK(input_owner==INPUT_DRAIN&&atomic_load(&wanted_view)==2);
    puts("input_test: PASS modal close needs a new view and remains draining after ACK");
}

static void *deliver_flip(void *ticket) {
    page_flip(-1,1,0,0,ticket);
    return NULL;
}

static void test_contended_ack(void) {
    reset_input(true,6);
    struct UiScene next=displayed_scene;
    next.page=1;next.epoch=11;next.serial=11;
    queue_scene(0,&next);
    atomic_store(&submitted_epoch,next.epoch);
    input_owner=INPUT_PANEL;gesture_action=ACTION_SELECT;
    CHECK(pthread_mutex_lock(&input_mutex)==0);
    pthread_t thread;
    CHECK(pthread_create(&thread,NULL,deliver_flip,&tickets[0])==0);
    CHECK(pthread_join(thread,NULL)==0);
    CHECK(atomic_load(&tickets[0].state)==TICKET_COMPLETE);
    CHECK(displayed_scene.serial==10&&atomic_load(&acknowledged_epoch)==10);
    CHECK(pthread_mutex_unlock(&input_mutex)==0);
    CHECK(pthread_mutex_lock(&input_mutex)==0);
    consume_completions();
    CHECK(pthread_mutex_unlock(&input_mutex)==0);
    CHECK(atomic_load(&tickets[0].state)==TICKET_FREE);
    CHECK(displayed_scene.serial==11&&displayed_scene.page==1);
    CHECK(atomic_load(&acknowledged_epoch)==11&&atomic_load(&presented_view)==1);
    CHECK(input_owner==INPUT_DRAIN&&gesture_action==ACTION_NONE);
    puts("input_test: PASS contended real flip ACK retained and later consumed");
}

static void test_serial_order(void) {
    reset_input(true,6);
    struct UiScene older=displayed_scene,newer=displayed_scene;
    older.view=3;older.epoch=11;older.serial=11;
    newer.view=5;newer.epoch=12;newer.serial=12;newer.page=1;
    newer.state.revision=43;
    queue_scene(0,&older);queue_scene(1,&newer);
    atomic_store(&wanted_view,newer.view);
    atomic_store(&submitted_epoch,newer.epoch);
    page_flip2(-1,2,0,0,0,&tickets[1]);
    CHECK(displayed_scene.serial==12&&atomic_load(&acknowledged_epoch)==12);
    page_flip(-1,1,0,0,&tickets[0]);
    CHECK(displayed_scene.serial==12&&displayed_scene.view==5&&displayed_scene.page==1);
    CHECK(displayed_scene.state.revision==43);
    CHECK(atomic_load(&presented_view)==5&&atomic_load(&acknowledged_epoch)==12);
    CHECK(atomic_load(&tickets[0].state)==TICKET_FREE);
    CHECK(atomic_load(&tickets[1].state)==TICKET_FREE);
    puts("input_test: PASS older flip serial cannot regress the presented scene");
}

static void test_binding_identity(void) {
    reset_input(true,6);
    struct UiScene a=displayed_scene,b=a;
    CHECK(same_bindings(&a,&b));
    b.epoch++;b.serial++;
    strcpy(b.state.presets[0].name,"RENAMED");
    strcpy(b.state.message,"A COSMETIC STATUS UPDATE");
    CHECK(same_bindings(&a,&b));
    b=a;b.page=1;CHECK(!same_bindings(&a,&b));
    b=a;strcpy(b.state.presets[0].id,"replacement");CHECK(!same_bindings(&a,&b));
    b=a;strcpy(b.selected,"preset-0");CHECK(!same_bindings(&a,&b));
    b=a;b.state.count--;CHECK(!same_bindings(&a,&b));
    b=a;b.state.revision++;CHECK(!same_bindings(&a,&b));
    b=a;b.state.generation++;CHECK(!same_bindings(&a,&b));
    b=a;b.state.ready=false;CHECK(!same_bindings(&a,&b));
    b=a;b.state.busy=true;CHECK(!same_bindings(&a,&b));
    b=a;b.state.can_enable=true;CHECK(!same_bindings(&a,&b));
    b=a;b.state.can_disable=false;CHECK(!same_bindings(&a,&b));
    b=a;b.view+=2;CHECK(!same_bindings(&a,&b));
    a.view=0;b.view=0;
    b.page=1;b.state.count=0;b.state.revision++;
    strcpy(b.selected,"other");
    CHECK(same_bindings(&a,&b)); /* Hidden data cannot change launcher bindings. */
    puts("input_test: PASS actual same_bindings distinguishes semantic and cosmetic changes");
}

static void test_pending_binding_epoch(void) {
    for(unsigned paging=0;paging<2;++paging) {
        reset_input(true,6);
        struct UiScene next=displayed_scene;
        if(paging)next.page=1;
        else {
            struct UiPreset swap=next.state.presets[0];
            next.state.presets[0]=next.state.presets[5];
            next.state.presets[5]=swap;
        }
        CHECK(!same_bindings(&displayed_scene,&next));
        /* Represent a submitted commit, not a replacement commit model. We
         * do not call ui_commit (a syscall hook). Keep view unchanged here to
         * isolate the production epoch guard from its separate view guard. */
        next.epoch=11;next.serial=11;
        page=next.page;
        queue_scene(0,&next);
        atomic_store(&submitted_epoch,next.epoch);
        tap(350,290);
        CHECK(input_owner==INPUT_DRAIN&&selected_id[0]=='\0');
        CHECK(gesture_action==ACTION_NONE&&gesture_command.id[0]=='\0');
        CHECK(displayed_scene.page==0&&atomic_load(&wanted_view)==1);
        CHECK(atomic_load(&acknowledged_epoch)==10&&atomic_load(&presented_view)==1);
        page_flip(-1,1,0,0,&tickets[0]);
        CHECK(displayed_scene.page==next.page&&atomic_load(&acknowledged_epoch)==11);
        CHECK(input_owner==INPUT_DRAIN);
        tap(350,290); /* Still quarantined until the hardware barrier. */
        CHECK(selected_id[0]=='\0'&&input_owner==INPUT_DRAIN);
        /* Independent post-barrier precondition; no touch_quiet syscall. */
        CHECK(contact_count()==0&&frame_count==0&&replay_cursor==replay_count);
        input_owner=INPUT_NONE;
        tap(350,290);
        CHECK(strcmp(selected_id,next.state.presets[next.page*5].id)==0);
        CHECK(strcmp(selected_id,"preset-0")!=0);
        CHECK(atomic_load(&wanted_view)==3&&input_owner==INPUT_DRAIN);
    }
    puts("input_test: PASS paging and reordered IDs quarantine unacknowledged epochs");
}

static void test_contact_replacement(void) {
    for(unsigned same_slot=0;same_slot<2;++same_slot) {
        reset_input(true,2);
        struct Report down=mt_down(0,71,350,290,BTN_TOUCH,true);
        send_report(&down,false);
        CHECK(gesture_action==ACTION_SELECT);
        struct Report replacement={0};
        add(&replacement,EV_ABS,ABS_MT_SLOT,0);
        add(&replacement,EV_ABS,ABS_MT_TRACKING_ID,-1);
        add(&replacement,EV_ABS,ABS_MT_SLOT,same_slot?0:1);
        // Include ID reuse: numeric equality must not revive an ended contact.
        add(&replacement,EV_ABS,ABS_MT_TRACKING_ID,same_slot?71:72);
        add(&replacement,EV_ABS,ABS_MT_POSITION_X,320);
        add(&replacement,EV_ABS,ABS_MT_POSITION_Y,100);
        send_report(&replacement,false);
        CHECK(input_owner==INPUT_PANEL&&gesture_action==ACTION_NONE);
        struct Report up=mt_up(same_slot?0:1,BTN_TOUCH);
        send_report(&up,false);
        CHECK(selected_id[0]=='\0'&&atomic_load(&wanted_view)==1);
    }
    puts("input_test: PASS sequential contact replacement and same-slot ID reuse cancel actions");
}
static void test_fault_revokes_input(void) {
    reset_input(true,1);
    atomic_store(&failed,true);
    struct Report down=mt_down(0,81,900,575,BTN_TOUCH,true);
    add(&down,EV_SYN,SYN_REPORT,0);
    memcpy(event_frame,down.events,down.count*sizeof *event_frame);frame_count=down.count;
    process_frame();frame_count=0;
    CHECK(input_owner==INPUT_DRAIN&&gesture_action==ACTION_NONE);
    CHECK(atomic_load(&wanted_view)==1&&replay_count==0);
    gesture_action=ACTION_CLOSE;finish_gesture(900,575);
    CHECK(atomic_load(&wanted_view)==1);
    atomic_store(&failed,false);
    puts("input_test: PASS display faults cannot leave stale input scenes actionable");
}
static void test_view_tag_content_atomicity(void) {
    reset_input(true,2);
    strcpy(drawing_scene.selected,"preset-0");strcpy(selected_id,"preset-1");
    atomic_store(&wanted_view,3);source_view=3;
    uint8_t *old_pixels=driver_pixels;
    driver_pixels=calloc(1,PIXELS);CHECK(driver_pixels!=NULL);
    CHECK(pthread_mutex_lock(&input_mutex)==0);
    draw_panel();
    CHECK(drawing_scene.view==1&&source_view==1);
    CHECK(strcmp(drawing_scene.selected,"preset-0")==0);
    CHECK(pthread_mutex_unlock(&input_mutex)==0);
    draw_panel();
    CHECK(drawing_scene.view==3&&source_view==3);
    CHECK(strcmp(drawing_scene.selected,"preset-1")==0);
    free(driver_pixels);driver_pixels=old_pixels;
    puts("input_test: PASS contended layout copies cannot acknowledge unpainted requested contents");
}
static void test_forked_helper_isolation(void) {
    reset_input(false,0);
    int fds[2];CHECK(pipe(fds)==0);
    int (*old_close)(int)=vendor_close;
    vendor_close=close;video_fd=fds[0];touch_fd=fds[0];enabled=true;gui_pid=getpid();
    // A fork child inherits this locked mutex, but not its owning thread.
    // Its FD cleanup must bypass GUI state instead of deadlocking or asking
    // the parent's real runtime supervisor to terminate the preview.
    CHECK(pthread_mutex_lock(&input_mutex)==0);
    pid_t child=fork();CHECK(child>=0);
    if(child==0) {
        int result=ui_close(fds[0]);
        _exit(result==0&&!atomic_load(&failed)&&!atomic_load(&close_requested)?0:1);
    }
    bool done=false;int status=0;
    for(unsigned i=0;i<100;++i) {
        if(waitpid(child,&status,WNOHANG)==child){done=true;break;}
        struct timespec pause={.tv_nsec=10000000};nanosleep(&pause,NULL);
    }
    CHECK(pthread_mutex_unlock(&input_mutex)==0);
    if(!done){kill(child,SIGKILL);CHECK(waitpid(child,&status,0)==child);}
    CHECK(done&&WIFEXITED(status)&&WEXITSTATUS(status)==0);
    CHECK(!atomic_load(&failed)&&!atomic_load(&close_requested));
    enabled=false;video_fd=-1;touch_fd=-1;vendor_close=old_close;
    CHECK(close(fds[0])==0&&close(fds[1])==0);
    puts("input_test: PASS forked vendor helpers bypass parent GUI mutexes and fault markers");
}
int main(void) {
    test_launcher_order();
    test_queued_coordinates_and_stock_replay();
    test_btn_left();
    test_multiple_contacts();
    test_modal_close();
    test_contended_ack();
    test_serial_order();
    test_binding_identity();
    test_pending_binding_epoch();
    test_contact_replacement();
    test_fault_revokes_input();
    test_view_tag_content_atomicity();
    test_forked_helper_isolation();
    puts("input_test: PASS actual input/presentation logic (not hardware validation)");
    return EXIT_SUCCESS;
}
