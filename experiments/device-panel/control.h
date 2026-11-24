#ifndef RC2FX_UI_CONTROL_H
#define RC2FX_UI_CONTROL_H
#include <stdbool.h>
#include <stdint.h>
#define UI_MAX_PRESETS 128
struct UiPreset {
  char id[49];
  char name[129];
};
struct UiSnapshot {
  bool connected, known, enabled, ready, applied, busy, can_disable, can_enable;
  uint64_t revision, generation;
  unsigned count;
  struct UiPreset presets[UI_MAX_PRESETS];
  char message[160];
};
struct UiCommand {
  unsigned op;
  bool enabled;
  uint64_t revision, generation;
  char id[49];
};
/* Socket I/O runs on a background thread so render, input, and audio threads
 * never block. */
bool ui_control_start(const char *socket_path);
bool ui_control_snapshot(struct UiSnapshot *out);
bool ui_control_submit(const struct UiCommand *command);
void ui_control_visible(bool visible);
/* Stop accepting or sending mutations after a display/input fault without
 * blocking. */
void ui_control_revoke(void);
#endif
