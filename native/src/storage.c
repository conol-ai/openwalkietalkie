/* SPDX-License-Identifier: MIT */
#include <rtthread.h>
#include <rtdevice.h>
#include <string.h>
#include "flashdb.h"
#include "drv_flash.h"
#include "bf0_sibles_nvds.h"
#include "openwalkietalkie.h"

#define OWT_UI_PREFS_KEY "OWT_UI_PREFS"
#define OWT_UI_PREFS_SIZE 64

bool owt_ui_prefs_read(uint8_t *bytes, size_t length)
{
    if (length != OWT_UI_PREFS_SIZE) return false;
    return sifli_nvds_flash_read(OWT_UI_PREFS_KEY, bytes, length) == length;
}

bool owt_ui_prefs_write(const uint8_t *bytes, size_t length)
{
    if (length != OWT_UI_PREFS_SIZE) return false;
    uint8_t saved[OWT_UI_PREFS_SIZE];
    if (owt_ui_prefs_read(saved, sizeof(saved)) && memcmp(saved, bytes, length) == 0)
        return true;
    rt_pm_hw_device_start();
    int result = sifli_nvds_flash_write(OWT_UI_PREFS_KEY, bytes, length);
    rt_pm_hw_device_stop();
    if (result != NVDS_OK || !owt_ui_prefs_read(saved, sizeof(saved)) ||
        memcmp(saved, bytes, length) != 0) {
        rt_kprintf("storage: UI preferences save failed (%d)\n", result);
        return false;
    }
    return true;
}

int __real_rt_flash_write(uint32_t address, const uint8_t *bytes, int length);
fdb_err_t __real_fdb_kv_set_blob(fdb_kvdb_t db, const char *key, fdb_blob_t blob);

int __wrap_rt_flash_write(uint32_t address, const uint8_t *bytes, int length)
{
    rt_pm_hw_device_start();
    int count = __real_rt_flash_write(address, bytes, length);
    if (count != length || memcmp((const void *)address, bytes, length) != 0)
        rt_kprintf("storage: write failed address=0x%08x length=%d result=%d\n", address, length, count);
    rt_pm_hw_device_stop();
    return count;
}

fdb_err_t __wrap_fdb_kv_set_blob(fdb_kvdb_t db, const char *key, fdb_blob_t blob)
{
    /* Covers allocation/erase as well as writes when bond storage changes. */
    rt_pm_hw_device_start();
    fdb_err_t result = __real_fdb_kv_set_blob(db, key, blob);
    rt_pm_hw_device_stop();
    if (result != FDB_NO_ERR)
        rt_kprintf("storage: save %s failed (%d)\n", key, result);
    return result;
}
