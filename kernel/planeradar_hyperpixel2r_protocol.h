// SPDX-License-Identifier: GPL-2.0-only
#ifndef PLANERADAR_HYPERPIXEL2R_PROTOCOL_H
#define PLANERADAR_HYPERPIXEL2R_PROTOCOL_H

#ifdef __KERNEL__
#include <linux/types.h>
typedef u8 hp2r_u8;
typedef u16 hp2r_u16;
#else
#include <stdint.h>
#include <stddef.h>
typedef uint8_t hp2r_u8;
typedef uint16_t hp2r_u16;
#endif

#define HP2R_WIDTH 480
#define HP2R_HEIGHT 480
#define HP2R_CLOCK_KHZ 19200
#define HP2R_HSYNC_START 490
#define HP2R_HSYNC_END 506
#define HP2R_HTOTAL 562
#define HP2R_VSYNC_START 495
#define HP2R_VSYNC_END 555
#define HP2R_VTOTAL 570
#define HP2R_MAX_DATA 16

struct hp2r_command {
    hp2r_u8 command;
    hp2r_u8 data_len;
    hp2r_u16 delay_ms;
    hp2r_u8 data[HP2R_MAX_DATA];
};

typedef int (*hp2r_word_sink)(void *context, hp2r_u16 word);

int hp2r_emit_command(
    const struct hp2r_command *command,
    hp2r_word_sink sink,
    void *context
);

extern const struct hp2r_command hp2r_prepare_commands[];
extern const size_t hp2r_prepare_command_count;
extern const struct hp2r_command hp2r_display_off_command;
extern const struct hp2r_command hp2r_sleep_command;

#endif
