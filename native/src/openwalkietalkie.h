#ifndef OPENWALKIETALKIE_H
#define OPENWALKIETALKIE_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Rust entry points: application thread only. */
uint32_t owt_core_init(void);
/* Reload preferences after BLE_POWER_ON_IND initializes the SDK's NVDS. */
void owt_core_load_ui_prefs(void);
bool owt_core_poll(uint32_t now_ms);
struct owt_region { uint16_t x0, y0, x1, y1; }; /* Exclusive, even end coordinates. */
bool owt_core_begin_frame(bool force);
/* Returned rectangles are physical panel coordinates; pixels already have
 * mounting rotation applied and retain a 480-pixel source stride. */
const uint8_t *owt_core_draw_next(uint8_t slot, struct owt_region *region);
/* index is a physical panel band, not a logical UI band. */
const uint8_t *owt_core_draw_splash_band(uint16_t index);
uint32_t owt_core_splash_duration(void);
/* Panel mounting built into the Rust core: 0 (LilyGo board) or 180 (hw-a,
 * hw-b), and the board itself: 0 LilyGo, 1 Rev A, 2 Rev B. */
uint16_t owt_core_rotation(void);
uint8_t owt_core_board(void);
void owt_core_hid_connected(bool connected);
void owt_core_bluetooth_rssi(int8_t rssi);
void owt_core_hid_report(const uint8_t *data, size_t len);
uint8_t owt_core_battery_percent(void);
/* Write cached battery state into the 38-byte battery diagnostic core tail.
 * Application thread only; this never starts ADC or charger transactions. */
void owt_core_battery_diagnostic(uint8_t *data);
enum owt_screen { OWT_SCREEN_ACTIVE, OWT_SCREEN_DIM, OWT_SCREEN_OFF };
uint8_t owt_core_screen(void);
uint8_t owt_core_brightness(void);
uint32_t owt_core_wait_ms(void);
void owt_core_frame_ready(void);
void owt_core_panel_slept(void);
void owt_core_wake(uint32_t now);

enum owt_message_type {
    OWT_BLE_READY, OWT_CONNECTED, OWT_DISCONNECTED, OWT_REPORT,
    OWT_CCCD, OWT_ENCRYPTED, OWT_BT_EVENT, OWT_WAKE_DISPLAY, OWT_DIAGNOSE,
    OWT_RSSI,
};
struct owt_message {
    uint8_t type, conn_idx;
    uint16_t event_type, event_id;
    uint32_t epoch;
    uint8_t length;
    uint8_t data[64];
};
bool owt_post(const struct owt_message *message);
void owt_wake(void);
void owt_bluetooth_start(void);
void owt_bluetooth_message(const struct owt_message *message);
void owt_bluetooth_battery(uint8_t percent);
void owt_bluetooth_poll(uint32_t now);
int owt_hid_notify(const uint8_t *data, size_t length);
void owt_hid_disconnect(void);
/* Device-owned UI preferences, a shared versioned/checksummed 64-byte record.
 * Application thread only. Missing/corrupt storage falls back to English. */
bool owt_ui_prefs_read(uint8_t *bytes, size_t length);
bool owt_ui_prefs_write(const uint8_t *bytes, size_t length);
void owt_board_start(void);
/* Rev A's six buttons and wheel on GPIOs: an edge since the last call, and the
 * raw levels as src/input/direct.rs expects them (bit 0 PTT PA24, 1 ENTER
 * PA25, 2 ESC PA26, 3 SHIFT PA27, 4 LEFT PA37, 5 RIGHT PA38, 6 encoder A
 * PA39, 7 encoder B PA40; a set bit is a high, released pin). The dev board
 * answers false and 0xFF. */
bool owt_inputs_asserted(void);
uint8_t owt_inputs_read(void);
#ifdef OWT_BOARD_HW_B
/* Rev B: the BQ24073's CHG output on PA33 reads low while the cell charges. */
bool owt_charger_charging(void);
#endif
void owt_audio_poll(uint32_t now_ms);
void owt_display_band(uint16_t index, const uint8_t *pixels);
void owt_display_region(const struct owt_region *region, const uint8_t *pixels);
bool owt_display_busy(void);
void owt_display_on(void);
void owt_display_enable(void);
void owt_display_sleep(void);
void owt_display_wake(void);
void owt_display_reset(bool asserted);
void owt_board_diagnose(void);
bool owt_diagnostic_report(const uint8_t *data, size_t len);
void owt_display_brightness(uint8_t level);
void owt_power_stats(uint32_t now, uint8_t screen);
void owt_idle_power_start(void);
void owt_idle_power_ui(bool active);
void owt_idle_power_stats(void);
#endif
