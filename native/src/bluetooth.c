/* SPDX-License-Identifier: MIT
 * GATT service uses SiFli's public service API (see SDK example/ble/hid).
 * HID identity and report format match the shared src/link/wire.rs protocol.
 */
#include <rtthread.h>
#include <string.h>
#include "bf0_ble_gap.h"
#include "bf0_sibles.h"
#include "bf0_sibles_advertising.h"
#include "bf0_sibles_nvds.h"
#include "ble_connection_manager.h"
#include "bts2_app_inc.h"
#include "openwalkietalkie.h"
#include "audio_server.h"
#include "speaker_pcm.h"

static const char name[] = "OpenWalkieTalkie";
/* Bluetooth Assigned Numbers: Audio Sink / Speakerphone. Native firmware is
 * a composite audio + HID device; a keyboard appearance can hide its output
 * from the Mac's Bluetooth route picker despite a valid Core Audio endpoint.
 * HID identification comes from its service, report map and DIS PnP ID. */
enum { AUDIO_APPEARANCE = 0x0845 };
static const uint8_t report_map[] = {
    0x06,0x00,0xff,0x09,0x01,0xa1,0x01,0x85,0x06,0x15,0x00,0x26,0xff,0x00,
    0x75,0x08,0x95,0x3f,0x09,0x01,0x81,0x02,0x95,0x3f,0x09,0x02,0x91,0x02,0xc0
};
static const uint8_t hid_info[] = {0x11, 0x01, 0, 2};
static const uint8_t input_ref[] = {6, 1}, output_ref[] = {6, 2};
/* USB VID source, VID 1209, PID 5754, release 0101 (little-endian). */
static const uint8_t pnp[] = {2, 0x09, 0x12, 0x54, 0x57, 1, 1};
static uint8_t protocol = 1, control_point, battery;
static uint8_t input_value[63], output_value[63];
static uint8_t cccd[2], battery_cccd[2];
static volatile uint8_t connection = 0xff;
static volatile uint32_t connection_epoch;
/* App-thread owned. A lost response clears the bars on the next poll. */
static uint32_t rssi_at;
static bool rssi_pending;
static sibles_hdl hid, dis, bas;
static char subscription_key[24];
static uint8_t persisted_cccd = 0xff;
SIBLES_ADVERTISING_CONTEXT_DECLAR(advertising);

enum {
    H_SVC, H_INFO_C, H_INFO, H_MAP_C, H_MAP, H_CONTROL_C, H_CONTROL,
    H_PROTOCOL_C, H_PROTOCOL, H_INPUT_C, H_INPUT, H_INPUT_REF, H_CCCD,
    H_OUTPUT_C, H_OUTPUT, H_OUTPUT_REF, H_COUNT
};
#define DECL {ATT_DECL_CHARACTERISTIC, PERM(RD, ENABLE), 0, 0}
#define READ(uuid, size) {uuid, PERM(RD, ENABLE), PERM(RI, ENABLE), size}
#define SERVICE {ATT_DECL_PRIMARY_SERVICE, PERM(RD, ENABLE), 0, 0}
static struct attm_desc hid_db[] = {
    [H_SVC] = SERVICE,
    [H_INFO_C] = DECL, [H_INFO] = READ(0x2a4a, 4),
    [H_MAP_C] = DECL, [H_MAP] = READ(0x2a4b, sizeof(report_map)),
    [H_CONTROL_C] = DECL,
    [H_CONTROL] = {0x2a4c, PERM(WRITE_COMMAND, ENABLE), PERM(RI, ENABLE), 1},
    [H_PROTOCOL_C] = DECL,
    [H_PROTOCOL] = {0x2a4e, PERM(RD, ENABLE) | PERM(WRITE_COMMAND, ENABLE), PERM(RI, ENABLE), 1},
    [H_INPUT_C] = DECL,
    [H_INPUT] = {0x2a4d, PERM(RD, ENABLE) | PERM(RP, UNAUTH) | PERM(NTF, ENABLE) |
                          PERM(NP, UNAUTH), PERM(RI, ENABLE), 63},
    [H_INPUT_REF] = READ(0x2908, 2),
    [H_CCCD] = {0x2902, PERM(RD, ENABLE) | PERM(WRITE_REQ, ENABLE) | PERM(WP, UNAUTH), PERM(RI, ENABLE), 2},
    [H_OUTPUT_C] = DECL,
    [H_OUTPUT] = {0x2a4d, PERM(RD, ENABLE) | PERM(RP, UNAUTH) | PERM(WRITE_REQ, ENABLE) |
                           PERM(WRITE_COMMAND, ENABLE) | PERM(WP, UNAUTH), PERM(RI, ENABLE), 64},
    [H_OUTPUT_REF] = READ(0x2908, 2),
};
static struct attm_desc dis_db[] = {
    SERVICE, DECL, READ(0x2a29, 11), DECL, READ(0x2a24, 11), DECL, READ(0x2a50, 7)
};
static struct attm_desc bas_db[] = {
    SERVICE, DECL, {0x2a19, PERM(RD, ENABLE) | PERM(NTF, ENABLE), PERM(RI, ENABLE), 1},
    {0x2902, PERM(RD, ENABLE) | PERM(WRITE_REQ, ENABLE), PERM(RI, ENABLE), 2}
};

static void disconnect_hid(void)
{
    uint8_t idx = connection;
    connection = 0xff;
    if (idx != 0xff)
    {
        ble_gap_disconnect_t request = {.conn_idx = idx, .reason = 0x13};
        ble_gap_disconnect(&request);
    }
}

/* Overflow recovery: the core has told the hosts everything is released and
 * wants a fresh link so that a lost release cannot stay held. */
void owt_hid_disconnect(void) { disconnect_hid(); }

static uint8_t *hid_get(uint8_t idx, uint8_t attr, uint16_t *length)
{
    (void)idx;
    if (attr == H_MAP || attr == H_INPUT_REF || attr == H_OUTPUT_REF)
        rt_kprintf("HID: read descriptor attr=%u\n", attr);
    *length = 0;
#define VALUE(a, v) case a: *length = sizeof(v); return (uint8_t *)(v)
    switch (attr) {
        VALUE(H_INFO, hid_info); VALUE(H_MAP, report_map);
        VALUE(H_INPUT, input_value); VALUE(H_OUTPUT, output_value);
        VALUE(H_INPUT_REF, input_ref); VALUE(H_OUTPUT_REF, output_ref);
        VALUE(H_CCCD, cccd);
        case H_PROTOCOL: *length = 1; return &protocol;
        case H_CONTROL: *length = 1; return &control_point;
        default: return NULL;
    }
}

static uint8_t hid_set(uint8_t idx, sibles_set_cbk_t *value)
{
    struct owt_message message = {.conn_idx = idx, .epoch = connection_epoch};
    rt_kprintf("HID: write attr=%u length=%u offset=%u\n", value->idx, value->len, value->offset);
    if (idx != connection) return 0x0e;
    if (value->offset) return 0x07;
    switch (value->idx) {
    case H_CCCD:
        if (value->len != 2 || value->value[1] || value->value[0] > 1) return 0x0d;
        message.type = OWT_CCCD; message.length = 1; message.data[0] = value->value[0];
        break;
    case H_OUTPUT:
        /* macOS can retain the report-ID byte in its GATT write. The shared
         * codec accepts both that 64-byte form and the 63-byte BLE body. */
        if (value->len < 2 || value->len > sizeof(message.data)) return 0x0d;
        message.type = OWT_REPORT; message.length = value->len;
        memcpy(message.data, value->value, value->len);
        break;
    case H_PROTOCOL:
        if (value->len != 1 || value->value[0] != 1) return 0x13;
        return 0;
    case H_CONTROL:
        if (value->len != 1 || value->value[0] > 1) return 0x0d;
        control_point = value->value[0]; return 0;
    default: return 0x03;
    }
    if (!owt_post(&message)) { disconnect_hid(); return 0x11; }
    return 0;
}

static uint8_t *dis_get(uint8_t idx, uint8_t attr, uint16_t *length)
{
    (void)idx;
    *length = 0;
    switch (attr) {
    case 2: *length = 5; return (uint8_t *)"Cohdl";
    case 4: *length = sizeof(name) - 1; return (uint8_t *)name;
    VALUE(6, pnp);
    default: return NULL;
    }
}
static uint8_t *bas_get(uint8_t idx, uint8_t attr, uint16_t *length)
{
    (void)idx;
    *length = attr == 2 ? 1 : attr == 3 ? 2 : 0;
    return attr == 2 ? &battery : attr == 3 ? battery_cccd : NULL;
}
static uint8_t bas_set(uint8_t idx, sibles_set_cbk_t *value)
{
    (void)idx;
    if (value->idx != 3 || value->len != 2 || value->value[1] || value->value[0] > 1) return 0x0d;
    battery_cccd[0] = value->value[0];
    return 0;
}
static uint8_t readonly_set(uint8_t idx, sibles_set_cbk_t *value)
{
    (void)idx; (void)value; return 0x03;
}

static sibles_hdl service(uint16_t uuid, struct attm_desc *db, unsigned count,
                          sibles_get_cbk get, sibles_set_cbk set)
{
    sibles_register_svc_t definition = { .uuid = uuid, .att_db = db, .num_entry = count,
        .sec_lvl = PERM(SVC_AUTH, NO_AUTH) | PERM(SVC_UUID_LEN, UUID_16) };
    sibles_hdl handle = sibles_register_svc(&definition);
    RT_ASSERT(handle);
    sibles_register_cbk(handle, get, set);
    return handle;
}

static uint8_t adv_event(uint8_t event, void *context, void *data)
{
    (void)data;
    if (event == SIBLES_ADV_EVT_ADV_STARTED) {
        sibles_advertising_context_t *adv = context;
        rt_kprintf("BLE: HID advertising interval=%ums\n",
                   adv->adv_para.prim_cfg.adv_intv_min * 625 / 1000);
    }
    return 0;
}

uint8_t sibles_advertising_disc_mode_get(void) { return GAPM_ADV_MODE_CUSTOMIZE; }

static void start_hid(void)
{
    sifli_hci_log_enable(false); /* NVDS is initialized after BLE_POWER_ON_IND. */
    bd_addr_t address;
    if (ble_get_public_address(&address) == HL_ERR_NO_ERROR)
        rt_kprintf("Bluetooth: %02X:%02X:%02X:%02X:%02X:%02X\n", address.addr[5], address.addr[4],
                   address.addr[3], address.addr[2], address.addr[1], address.addr[0]);
    connection_manager_set_bond_cnf_iocap(GAP_IO_CAP_NO_INPUT_NO_OUTPUT);
    connection_manager_set_bond_cnf_auth(GAP_AUTH_REQ_SEC_CON_BOND);
    connection_manager_set_bond_cnf_sec(GAP_SEC1_NOAUTH_PAIR_ENC);
    connection_manager_set_bond_cnf_accept(true);
    dis = service(0x180a, dis_db, sizeof(dis_db)/sizeof(dis_db[0]), dis_get, readonly_set);
    bas = service(0x180f, bas_db, sizeof(bas_db)/sizeof(bas_db[0]), bas_get, bas_set);
    hid = service(0x1812, hid_db, H_COUNT, hid_get, hid_set);
    uint8_t name_buffer[sizeof(ble_gap_dev_name_t) + sizeof(name)] = {0};
    ble_gap_dev_name_t *dev_name = (ble_gap_dev_name_t *)name_buffer;
    dev_name->len = sizeof(name) - 1;
    memcpy(dev_name->name, name, dev_name->len);
    ble_gap_set_dev_name(dev_name);
    uint16_t appearance = AUDIO_APPEARANCE;
    ble_gap_set_appearance(appearance);
    uint8_t adv_name_buffer[sizeof(sibles_adv_type_name_t) + sizeof(name)] = {0};
    sibles_adv_type_name_t *adv_name = (sibles_adv_type_name_t *)adv_name_buffer;
    adv_name->name_len = sizeof(name) - 1;
    memcpy(adv_name->name, name, adv_name->name_len);
    uint8_t uuid_buffer[sizeof(sibles_adv_type_srv_uuid_t) + 8] = {0};
    sibles_adv_type_srv_uuid_t *uuids = (sibles_adv_type_srv_uuid_t *)uuid_buffer;
    uuids->count = 1;
    uuids->uuid_list[0].uuid_len = 2;
    uuids->uuid_list[0].uuid.uuid_16[0] = 0x12;
    uuids->uuid_list[0].uuid.uuid_16[1] = 0x18;
    sibles_advertising_para_t para = {0};
    para.own_addr_type = GAPM_STATIC_ADDR;
    para.config.adv_mode = SIBLES_ADV_CONNECT_MODE;
    para.config.mode_config.conn_config.interval = 0x60;
    /* SDK-managed fast discovery for 30s, then indefinite 1s background
     * advertising. Disconnect restarts the fast phase; existing links and
     * Classic audio/sniff policy are unaffected. Durations use 10ms units. */
    para.config.mode_config.conn_config.duration = 3000;
    para.config.mode_config.conn_config.backgroud_mode_enabled = 1;
    para.config.mode_config.conn_config.backgroud_interval = 1600;
    para.config.mode_config.conn_config.backgroud_duration = 0;
    para.config.is_auto_restart = 1;
    para.config.max_tx_pwr = 0x7f;
    /* General discovery + simultaneous LE/BR-EDR controller and host.
     * The SDK's default BLE-only flags hide the Classic audio transport. */
    para.adv_data.flags = 0x1a;
    para.adv_data.completed_uuid = uuids;
    para.adv_data.appearance = &appearance;
    para.rsp_data.completed_name = adv_name;
    para.evt_handler = adv_event;
    RT_ASSERT(sibles_advertising_init(advertising, &para) == SIBLES_ADV_NO_ERR);
    sibles_advertising_start(advertising);
}

static void restore_subscription(void)
{
    ble_gap_addr_t peer;
    if (connection == 0xff || connection_manager_get_enc_state(connection) != ENC_STATE_ON) return;
    if (connection_manager_get_addr_by_conn_idx(connection, &peer) != CM_STATUS_OK) return;
    rt_snprintf(subscription_key, sizeof(subscription_key), "OWT_H%02x%02x%02x%02x%02x%02x%02x",
                peer.addr_type, peer.addr.addr[0], peer.addr.addr[1], peer.addr.addr[2],
                peer.addr.addr[3], peer.addr.addr[4], peer.addr.addr[5]);
    uint8_t saved = 0;
    if (sifli_nvds_flash_read(subscription_key, &saved, 1) == 1 && saved <= 1)
        cccd[0] = saved;
    persisted_cccd = cccd[0];
}

int owt_hid_notify(const uint8_t *data, size_t length)
{
    uint32_t deadline = rt_tick_get_millisecond() + 250;
    if (!hid || length != 63) return -1;
    while (connection != 0xff && cccd[0] == 1 &&
           connection_manager_get_enc_state(connection) == ENC_STATE_ON)
    {
        sibles_value_t value = {.hdl = hid, .idx = H_INPUT, .len = length, .value = (uint8_t *)data};
        if (sibles_write_value(connection, &value) == (int)length) return 0;
        if ((int32_t)(rt_tick_get_millisecond() - deadline) >= 0) break;
        rt_thread_mdelay(2);
    }
    /* A lost release must not leave the host holding PTT indefinitely. */
    disconnect_hid();
    return -1;
}

void owt_bluetooth_battery(uint8_t percent)
{
    battery = percent;
    if (bas && connection != 0xff && battery_cccd[0]) {
        sibles_value_t value = {.hdl = bas, .idx = 2, .len = 1, .value = &battery};
        sibles_write_value(connection, &value);
    }
}

static int ble_event(uint16_t event, uint8_t *data, uint16_t length, uint32_t context)
{
    (void)length; (void)context;
    struct owt_message message = {0};
    switch (event) {
    case BLE_POWER_ON_IND: message.type = OWT_BLE_READY; break;
    case BLE_GAP_CONNECTED_IND:
        {
            ble_gap_connect_ind_t *link = (ble_gap_connect_ind_t *)data;
            rt_kprintf("BLE: parameters interval_us=%u latency=%u timeout_ms=%u\n",
                       link->con_interval * 1250, link->con_latency, link->sup_to * 10);
        }
        connection_epoch++;
        connection = ((ble_gap_connect_ind_t *)data)->conn_idx;
        message.type = OWT_CONNECTED; message.conn_idx = connection; break;
    case BLE_GAP_DISCONNECTED_IND:
        connection_epoch++;
        message.type = OWT_DISCONNECTED;
        message.data[0] = ((ble_gap_disconnected_ind_t *)data)->reason;
        connection = 0xff; break;
    case BLE_GAP_ENCRYPT_IND:
        rt_kprintf("BLE: link encrypted\n");
        message.type = OWT_ENCRYPTED; message.conn_idx = connection; break;
    case BLE_GAP_REMOTE_RSSI_IND:
        {
            const ble_gap_remote_rssi_ind_t *sample = (const void *)data;
            if (sample->conn_idx != connection || connection == 0xff) return 0;
            message.type = OWT_RSSI;
            message.conn_idx = sample->conn_idx;
            message.data[0] = (uint8_t)sample->rssi;
            message.epoch = connection_epoch;
            /* A missed RSSI update must not disconnect a healthy HID link. */
            owt_post(&message);
            return 0;
        }
    case SIBLES_MTU_EXCHANGE_IND:
        rt_kprintf("BLE: MTU=%u\n", ((sibles_mtu_exchange_ind_t *)data)->mtu);
        return 0;
    case BLE_GAP_UPDATE_CONN_PARAM_IND:
        {
            ble_gap_update_conn_param_ind_t *link = (ble_gap_update_conn_param_ind_t *)data;
            rt_kprintf("BLE: parameters updated interval_us=%u latency=%u timeout_ms=%u\n",
                       link->con_interval * 1250, link->con_latency, link->sup_to * 10);
        }
        return 0;
    case SIBLES_WRITE_VALUE_RSP:
        if (((sibles_write_value_rsp_t *)data)->result) {
            rt_kprintf("HID: notify failed result=%u\n", ((sibles_write_value_rsp_t *)data)->result);
            disconnect_hid();
        }
        return 0;
    default: return 0;
    }
    message.epoch = connection_epoch;
    if (!owt_post(&message)) disconnect_hid();
    return 0;
}
BLE_EVENT_REGISTER(ble_event, NULL);

static int bt_event(uint16_t type, uint16_t event, uint8_t *data, uint16_t length)
{
    /* SDK workers own audio payloads. Never enqueue per-packet A2DP data (or
     * transient pointers to it) behind touch/HID control messages. */
    if (type == BT_NOTIFY_A2DP && event == BT_NOTIFY_A2DP_DATA_IND) return 0;
    struct owt_message message = {.type = OWT_BT_EVENT, .event_type = type, .event_id = event};
    message.length = length < sizeof(message.data) ? length : sizeof(message.data);
    if (message.length) memcpy(message.data, data, message.length);
    owt_post(&message);
    return 0;
}

void owt_bluetooth_start(void)
{
    bt_interface_register_bt_event_notify_callback(bt_event);
    sifli_ble_enable();
}

void owt_bluetooth_poll(uint32_t now)
{
    if (connection == 0xff || now - rssi_at < 3000) return;
    rssi_at = now;
    if (rssi_pending) owt_core_bluetooth_rssi(127);
    ble_gap_get_rssi_t request = {.conn_idx = connection};
    rssi_pending = ble_gap_get_remote_rssi(&request) == 0;
    if (!rssi_pending) owt_core_bluetooth_rssi(127);
}

/* SDK hook runs after NVDS initialization and before controller startup. */
ble_common_update_type_t ble_request_public_address(bd_addr_t *address)
{
    uint8_t uid[16];
    *address = (bd_addr_t){{0xc2, 0x53, 0x46, 0x32, 0x52, 0xc0}};
    RT_ASSERT(HAL_EFUSE_Init() == HAL_OK);
    RT_ASSERT(HAL_EFUSE_Read(0, uid, sizeof(uid)) == sizeof(uid));
    for (unsigned i = 0; i < sizeof(uid); i++) address->addr[i % 6] ^= uid[i];
    address->addr[5] |= 0xc0;
    return BLE_UPDATE_ALWAYS;
}

/* Audio/video wearable headset: advertise capture as well as rendering. */
uint32_t bt_get_class_of_device(void) { return 0x2c0404; }

void owt_bluetooth_message(const struct owt_message *message)
{
    if (message->type != OWT_BLE_READY && message->type != OWT_BT_EVENT &&
        message->epoch != connection_epoch) return;
    switch (message->type) {
    case OWT_BLE_READY: owt_core_load_ui_prefs(); start_hid(); break;
    case OWT_CONNECTED:
        cccd[0] = battery_cccd[0] = 0; subscription_key[0] = 0; persisted_cccd = 0xff;
        rssi_pending = false;
        rssi_at = rt_tick_get_millisecond() - 3000;
        owt_core_hid_connected(true);
        rt_kprintf("BLE: connected\n"); break;
    case OWT_DISCONNECTED:
        rssi_pending = false;
        owt_core_hid_connected(false);
        rt_kprintf("BLE: disconnected reason=0x%02x\n", message->data[0]); break;
    case OWT_ENCRYPTED:
        restore_subscription();
        break;
    case OWT_RSSI:
        if (message->conn_idx != connection || !rssi_pending) break;
        rssi_pending = false;
        owt_core_bluetooth_rssi((int8_t)message->data[0]);
        break;
    case OWT_CCCD:
        if (message->conn_idx != connection) break;
        if (!subscription_key[0]) restore_subscription();
        cccd[0] = message->data[0];
        if (subscription_key[0] && persisted_cccd != cccd[0]) {
            if (sifli_nvds_flash_write(subscription_key, cccd, 1) == NVDS_OK)
                persisted_cccd = cccd[0];
            else rt_kprintf("BLE: subscription save failed\n");
        }
        break;
    case OWT_REPORT:
        if (message->conn_idx == connection && connection_manager_get_enc_state(connection) == ENC_STATE_ON) {
            if (!owt_diagnostic_report(message->data, message->length))
                owt_core_hid_report(message->data, message->length);
        }
        break;
    case OWT_BT_EVENT:
        if (message->event_type == BT_NOTIFY_COMMON) {
            switch (message->event_id) {
            case BT_NOTIFY_COMMON_BT_STACK_READY:
                bt_interface_set_local_name(sizeof(name) - 1, (void *)name);
                {
                    /* The SDK name setter replaces EIR. Restore HFP, Audio Sink,
                     * AVRCP and Generic Audio service UUIDs. */
                    uint8_t eir[sizeof(name) + 13] = {sizeof(name), 0x09};
                    memcpy(&eir[2], name, sizeof(name) - 1);
                    const uint8_t services[] = {11, 0x03, 0x1e, 0x11, 0x0b, 0x11,
                                               0x0c, 0x11, 0x0e, 0x11, 0x03, 0x12};
                    memcpy(&eir[sizeof(name) + 1], services, sizeof(services));
                    bt_interface_set_eir_data(0, sizeof(eir), eir);
                }
                bt_interface_set_scan_mode(1, 1);
                rt_kprintf("audio: HFP + A2DP sink ready, enabling Classic discovery\n"); break;
            case BT_NOTIFY_COMMON_SCAN_ENB_CFM_IND:
                if (message->length) rt_kprintf("HFP: Classic scan mode=%u\n", message->data[0]);
                break;
            case BT_NOTIFY_COMMON_ACL_CONNECTED:
                if (message->length >= sizeof(bt_notify_device_acl_conn_info_t)) {
                    bt_notify_device_acl_conn_info_t info;
                    memcpy(&info, message->data, sizeof(info));
                    rt_kprintf("HFP: Classic connection result=%u\n", info.res);
                }
                break;
            case BT_NOTIFY_COMMON_PAIR_IND: rt_kprintf("HFP: Classic pairing event\n"); break;
            case BT_NOTIFY_COMMON_SCO_CONNECTED:
                if (message->length >= sizeof(bt_notify_device_sco_info_t)) {
                    bt_notify_device_sco_info_t sco;
                    memcpy(&sco, message->data, sizeof(sco));
                    /* Without super-wideband the SDK reports codec_id=0xff;
                     * its audio path then selects CVSD/mSBC from air_mode. */
                    unsigned rate = sco.para.codec_id == 3 ? 32000 :
                                    sco.para.air_mode == 2 ? 8000 : 16000;
                    rt_kprintf("HFP: voice stream result=%u codec=%u air=%u rate=%u\n",
                               sco.sco_res, sco.para.codec_id, sco.para.air_mode, rate);
                }
                break;
            case BT_NOTIFY_COMMON_SCO_DISCONNECTED: rt_kprintf("HFP: voice stream closed\n"); break;
            default: break;
            }
        } else if (message->event_type == BT_NOTIFY_HFP_HF) {
            if (message->event_id == BT_NOTIFY_HF_PROFILE_CONNECTED) {
                rt_kprintf("HFP: Mac profile connected\n");
            }
            if (message->event_id == BT_NOTIFY_HF_PROFILE_DISCONNECTED) {
                rt_kprintf("HFP: profile disconnected\n");
            }
        } else if (message->event_type == BT_NOTIFY_A2DP) {
            switch (message->event_id) {
            case BT_NOTIFY_A2DP_PROFILE_CONNECTED: rt_kprintf("A2DP: Mac playback connected\n"); break;
            case BT_NOTIFY_A2DP_PROFILE_DISCONNECTED: rt_kprintf("A2DP: playback disconnected\n"); break;
            case BT_NOTIFY_A2DP_START_IND: rt_kprintf("A2DP: playback started\n"); break;
            case BT_NOTIFY_A2DP_SUSPEND_IND: rt_kprintf("A2DP: playback suspended\n"); break;
            default: break;
            }
        } else if (message->event_type == BT_NOTIFY_AVRCP &&
                   message->event_id == BT_NOTIFY_AVRCP_ABSOLUTE_VOLUME && message->length == 1) {
            uint8_t volume = owt_speaker_volume(message->data[0], audio_server_get_max_volume());
            audio_server_set_private_volume(AUDIO_TYPE_BT_MUSIC, volume);
            rt_kprintf("speaker: Mac volume=%u local=%u\n", message->data[0], volume);
        }
        break;
    default: break;
    }
}
