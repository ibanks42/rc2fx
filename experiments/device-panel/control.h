#ifndef RC2FX_UI_CONTROL_H
#define RC2FX_UI_CONTROL_H
#include <stdbool.h>
#include <stdint.h>
#define UI_MAX_PRESETS 128
struct UiPreset { char id[49]; char name[129]; };
struct UiSnapshot {
    bool connected, known, enabled, ready, applied, busy, can_disable, can_enable;
    uint64_t revision, generation;
    unsigned count;
    struct UiPreset presets[UI_MAX_PRESETS];
    char message[160];
};
struct UiCommand { unsigned op; bool enabled; uint64_t revision, generation; char id[49]; };
/* All transport I/O is on its own thread, never on rendering/input/audio. */
bool ui_control_start(const char *socket_path);
bool ui_control_snapshot(struct UiSnapshot *out);
bool ui_control_submit(const struct UiCommand *command);
void ui_control_visible(bool visible);
/* Nonblocking revocation after display/input failure; queued writes are dropped. */
void ui_control_revoke(void);
#endif
