// SPDX-License-Identifier: GPL-2.0-only
#include "planeradar_hyperpixel2r_protocol.h"

const struct hp2r_command hp2r_prepare_commands[] = {
    { .command = 0x01, .data_len = 1, .delay_ms = 5, .data = { 0x00 } },
    { .command = 0x11, .data_len = 1, .delay_ms = 120, .data = { 0x00 } },
    { .command = 0xff, .data_len = 5, .delay_ms = 0,
      .data = { 0x77, 0x01, 0x00, 0x00, 0x10 } },
    { .command = 0xb0, .data_len = 16, .delay_ms = 0,
      .data = { 0x02, 0x13, 0x1b, 0x0d, 0x10, 0x05, 0x08, 0x07,
                0x07, 0x24, 0x04, 0x11, 0x0e, 0x2c, 0x33, 0x1d } },
    { .command = 0xb1, .data_len = 16, .delay_ms = 0,
      .data = { 0x05, 0x13, 0x1b, 0x0d, 0x11, 0x05, 0x08, 0x07,
                0x07, 0x24, 0x04, 0x11, 0x0e, 0x2c, 0x33, 0x1d } },
    { .command = 0xc0, .data_len = 2, .delay_ms = 0,
      .data = { 0x3b, 0x00 } },
    { .command = 0xc1, .data_len = 2, .delay_ms = 0,
      .data = { 0x0f, 0x0f } },
    { .command = 0xc2, .data_len = 2, .delay_ms = 0,
      .data = { 0x30, 0x03 } },
    { .command = 0xff, .data_len = 5, .delay_ms = 0,
      .data = { 0x77, 0x01, 0x00, 0x00, 0x11 } },
    { .command = 0xb0, .data_len = 1, .delay_ms = 0, .data = { 0x12 } },
    { .command = 0xb1, .data_len = 1, .delay_ms = 0, .data = { 0x25 } },
    { .command = 0xb2, .data_len = 1, .delay_ms = 0, .data = { 0x00 } },
    { .command = 0xb3, .data_len = 1, .delay_ms = 0, .data = { 0x80 } },
    { .command = 0xb5, .data_len = 1, .delay_ms = 0, .data = { 0x40 } },
    { .command = 0xb7, .data_len = 1, .delay_ms = 0, .data = { 0x00 } },
    { .command = 0xb8, .data_len = 1, .delay_ms = 0, .data = { 0x12 } },
    { .command = 0xc1, .data_len = 1, .delay_ms = 0, .data = { 0x70 } },
    { .command = 0xc2, .data_len = 1, .delay_ms = 0, .data = { 0x7b } },
    { .command = 0xd0, .data_len = 1, .delay_ms = 0, .data = { 0x80 } },

    { .command = 0x01, .data_len = 0, .delay_ms = 5 },
    { .command = 0xff, .data_len = 5, .delay_ms = 0,
      .data = { 0x77, 0x01, 0x00, 0x00, 0x10 } },
    { .command = 0xc0, .data_len = 2, .delay_ms = 0,
      .data = { 0x3b, 0x00 } },
    { .command = 0xc1, .data_len = 2, .delay_ms = 0,
      .data = { 0x0b, 0x02 } },
    { .command = 0xc2, .data_len = 2, .delay_ms = 0,
      .data = { 0x00, 0x02 } },
    { .command = 0xcc, .data_len = 1, .delay_ms = 0, .data = { 0x10 } },
    { .command = 0xff, .data_len = 5, .delay_ms = 0,
      .data = { 0x77, 0x01, 0x00, 0x00, 0x11 } },
    { .command = 0xb0, .data_len = 1, .delay_ms = 0, .data = { 0x5d } },
    { .command = 0xb1, .data_len = 1, .delay_ms = 0, .data = { 0x43 } },
    { .command = 0xb2, .data_len = 1, .delay_ms = 0, .data = { 0x81 } },
    { .command = 0xb3, .data_len = 1, .delay_ms = 0, .data = { 0x80 } },
    { .command = 0xb5, .data_len = 1, .delay_ms = 0, .data = { 0x43 } },
    { .command = 0xb7, .data_len = 1, .delay_ms = 0, .data = { 0x85 } },
    { .command = 0xb8, .data_len = 1, .delay_ms = 0, .data = { 0x20 } },
    { .command = 0xc1, .data_len = 1, .delay_ms = 0, .data = { 0x78 } },
    { .command = 0xc2, .data_len = 1, .delay_ms = 0, .data = { 0x78 } },
    { .command = 0xd0, .data_len = 1, .delay_ms = 0, .data = { 0x88 } },
    { .command = 0xe0, .data_len = 3, .delay_ms = 0,
      .data = { 0x00, 0x00, 0x02 } },
    { .command = 0xe1, .data_len = 11, .delay_ms = 0,
      .data = { 0x03, 0xa0, 0x00, 0x00, 0x04, 0xa0, 0x00, 0x00,
                0x00, 0x20, 0x20 } },
    { .command = 0xe2, .data_len = 13, .delay_ms = 0,
      .data = { 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00 } },
    { .command = 0xe3, .data_len = 4, .delay_ms = 0,
      .data = { 0x00, 0x00, 0x11, 0x00 } },
    { .command = 0xe4, .data_len = 2, .delay_ms = 0,
      .data = { 0x22, 0x00 } },
    { .command = 0xe5, .data_len = 16, .delay_ms = 0,
      .data = { 0x05, 0xec, 0xa0, 0xa0, 0x07, 0xee, 0xa0, 0xa0,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00 } },
    { .command = 0xe6, .data_len = 4, .delay_ms = 0,
      .data = { 0x00, 0x00, 0x11, 0x00 } },
    { .command = 0xe7, .data_len = 2, .delay_ms = 0,
      .data = { 0x22, 0x00 } },
    { .command = 0xe8, .data_len = 16, .delay_ms = 0,
      .data = { 0x06, 0xed, 0xa0, 0xa0, 0x08, 0xef, 0xa0, 0xa0,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00 } },
    { .command = 0xeb, .data_len = 7, .delay_ms = 0,
      .data = { 0x00, 0x00, 0x40, 0x40, 0x00, 0x00, 0x00 } },
    { .command = 0xed, .data_len = 16, .delay_ms = 0,
      .data = { 0xff, 0xff, 0xff, 0xba, 0x0a, 0xbf, 0x45, 0xff,
                0xff, 0x54, 0xfb, 0xa0, 0xab, 0xff, 0xff, 0xff } },
    { .command = 0xef, .data_len = 6, .delay_ms = 0,
      .data = { 0x10, 0x0d, 0x04, 0x08, 0x3f, 0x1f } },
    { .command = 0xff, .data_len = 5, .delay_ms = 0,
      .data = { 0x77, 0x01, 0x00, 0x00, 0x13 } },
    { .command = 0xef, .data_len = 1, .delay_ms = 0, .data = { 0x08 } },
    { .command = 0xff, .data_len = 5, .delay_ms = 0,
      .data = { 0x77, 0x01, 0x00, 0x00, 0x00 } },
    { .command = 0xcd, .data_len = 1, .delay_ms = 0, .data = { 0x08 } },
    { .command = 0x36, .data_len = 1, .delay_ms = 0, .data = { 0x08 } },
    { .command = 0x3a, .data_len = 1, .delay_ms = 0, .data = { 0x66 } },
    { .command = 0x11, .data_len = 0, .delay_ms = 0 },
    { .command = 0x29, .data_len = 1, .delay_ms = 0, .data = { 0x00 } },
};

const size_t hp2r_prepare_command_count =
    sizeof(hp2r_prepare_commands) / sizeof(hp2r_prepare_commands[0]);

const struct hp2r_command hp2r_display_off_command = {
    .command = 0x28,
    .data_len = 1,
    .delay_ms = 0,
    .data = { 0x00 },
};

const struct hp2r_command hp2r_sleep_command = {
    .command = 0x10,
    .data_len = 1,
    .delay_ms = 120,
    .data = { 0x00 },
};

int hp2r_emit_command(
    const struct hp2r_command *command,
    hp2r_word_sink sink,
    void *context
)
{
    size_t index;
    int result;

    if (!command || !sink || command->data_len > HP2R_MAX_DATA)
        return -1;

    result = sink(context, command->command);
    if (result)
        return result;

    for (index = 0; index < command->data_len; index++) {
        result = sink(context, 0x100u | command->data[index]);
        if (result)
            return result;
    }

    return 0;
}
