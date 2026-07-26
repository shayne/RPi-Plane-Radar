// SPDX-License-Identifier: GPL-2.0-only
#include <stdio.h>

#include "planeradar_hyperpixel2r_protocol.h"

struct word_capture {
    hp2r_u16 words[HP2R_MAX_DATA + 2];
    size_t count;
};

struct failing_sink {
    struct word_capture capture;
    size_t fail_on_call;
    int error;
};

static int capture_word(void *context, hp2r_u16 word)
{
    struct word_capture *capture = context;

    capture->words[capture->count++] = word;
    return 0;
}

static int fail_on_selected_call(void *context, hp2r_u16 word)
{
    struct failing_sink *sink = context;

    sink->capture.words[sink->capture.count++] = word;
    if (sink->capture.count == sink->fail_on_call)
        return sink->error;

    return 0;
}

static int expect(int condition, const char *message)
{
    if (condition)
        return 0;

    fprintf(stderr, "assertion failed: %s\n", message);
    return 1;
}

static const struct hp2r_command expected_prepare_commands[] = {
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

static int test_prepare_sequence_is_byte_exact(void)
{
    const size_t expected_count = sizeof(expected_prepare_commands) /
                                  sizeof(expected_prepare_commands[0]);
    size_t command_index;

    if (expect(hp2r_prepare_command_count == expected_count,
               "prepare command count is exact"))
        return 1;

    for (command_index = 0; command_index < expected_count; command_index++) {
        const struct hp2r_command *actual =
            &hp2r_prepare_commands[command_index];
        const struct hp2r_command *expected =
            &expected_prepare_commands[command_index];
        size_t data_index;

        if (actual->command != expected->command ||
            actual->data_len != expected->data_len ||
            actual->delay_ms != expected->delay_ms) {
            fprintf(stderr,
                    "prepare command %zu differs: got cmd=0x%02x len=%u "
                    "delay=%u, expected cmd=0x%02x len=%u delay=%u\n",
                    command_index, actual->command, actual->data_len,
                    actual->delay_ms, expected->command, expected->data_len,
                    expected->delay_ms);
            return 1;
        }

        for (data_index = 0; data_index < expected->data_len; data_index++) {
            if (actual->data[data_index] != expected->data[data_index]) {
                fprintf(stderr,
                        "prepare command %zu payload byte %zu differs: "
                        "got 0x%02x, expected 0x%02x\n",
                        command_index, data_index, actual->data[data_index],
                        expected->data[data_index]);
                return 1;
            }
        }
    }

    return 0;
}

static int test_null_command_and_sink_are_rejected(void)
{
    const struct hp2r_command command = { .command = 0x11 };
    struct word_capture capture = { 0 };

    if (expect(hp2r_emit_command(NULL, capture_word, &capture) == -1,
               "null command is rejected"))
        return 1;
    if (expect(capture.count == 0, "null command does not invoke sink"))
        return 1;

    return expect(hp2r_emit_command(&command, NULL, &capture) == -1,
                  "null sink is rejected");
}

static int test_oversize_data_is_rejected_without_invoking_sink(void)
{
    const struct hp2r_command command = {
        .command = 0xff,
        .data_len = HP2R_MAX_DATA + 1,
    };
    struct word_capture capture = { 0 };

    if (expect(hp2r_emit_command(&command, capture_word, &capture) == -1,
               "oversize data is rejected"))
        return 1;

    return expect(capture.count == 0,
                  "oversize data does not invoke the sink");
}

static int test_sink_errors_propagate_and_stop_immediately(void)
{
    const struct hp2r_command command = {
        .command = 0x3a,
        .data_len = 2,
        .data = { 0x66, 0x77 },
    };
    struct failing_sink command_failure = {
        .fail_on_call = 1,
        .error = -5,
    };
    struct failing_sink data_failure = {
        .fail_on_call = 2,
        .error = -9,
    };

    if (expect(hp2r_emit_command(&command, fail_on_selected_call,
                                 &command_failure) == -5,
               "command sink error is propagated"))
        return 1;
    if (expect(command_failure.capture.count == 1,
               "command sink error stops before data"))
        return 1;

    if (expect(hp2r_emit_command(&command, fail_on_selected_call,
                                 &data_failure) == -9,
               "data sink error is propagated"))
        return 1;

    return expect(data_failure.capture.count == 2 &&
                      data_failure.capture.words[0] == 0x03a &&
                      data_failure.capture.words[1] == 0x166,
                  "data sink error stops before the next data byte");
}

static int test_command_0x11_becomes_0x011(void)
{
    const struct hp2r_command command = { .command = 0x11 };
    struct word_capture capture = { 0 };

    if (expect(hp2r_emit_command(&command, capture_word, &capture) == 0,
               "emit exit-sleep command"))
        return 1;

    return expect(capture.count == 1 && capture.words[0] == 0x011,
                  "0x11 becomes command word 0x011");
}

static int test_data_0x77_becomes_0x177(void)
{
    const struct hp2r_command command = {
        .command = 0xff,
        .data_len = 1,
        .data = { 0x77 },
    };
    struct word_capture capture = { 0 };

    if (expect(hp2r_emit_command(&command, capture_word, &capture) == 0,
               "emit command-bank data"))
        return 1;

    return expect(capture.count == 2 && capture.words[1] == 0x177,
                  "0x77 becomes data word 0x177");
}

static int test_all_nine_bits_are_most_significant_bit_first(void)
{
    const struct hp2r_command command = {
        .command = 0,
        .data_len = 1,
        .data = { 0xaa },
    };
    const hp2r_u8 expected_bits[] = { 1, 1, 0, 1, 0, 1, 0, 1, 0 };
    struct word_capture capture = { 0 };
    size_t index;

    if (expect(hp2r_emit_command(&command, capture_word, &capture) == 0,
               "emit nine-bit data word"))
        return 1;
    if (expect(capture.count == 2, "capture data word"))
        return 1;

    for (index = 0; index < sizeof(expected_bits) / sizeof(expected_bits[0]);
         index++) {
        hp2r_u8 bit = (capture.words[1] >> (8 - index)) & 1u;

        if (expect(bit == expected_bits[index],
                   "data word keeps all nine bits most-significant-bit first"))
            return 1;
    }

    return 0;
}

static int test_first_prepare_command_is_soft_reset_with_5ms_delay(void)
{
    return expect(hp2r_prepare_command_count > 0 &&
                      hp2r_prepare_commands[0].command == 0x01 &&
                      hp2r_prepare_commands[0].data_len == 1 &&
                      hp2r_prepare_commands[0].data[0] == 0x00 &&
                      hp2r_prepare_commands[0].delay_ms == 5,
                  "first prepare command is soft reset with a 5 ms delay");
}

static int test_exit_sleep_has_120ms_delay(void)
{
    return expect(hp2r_prepare_command_count > 1 &&
                      hp2r_prepare_commands[1].command == 0x11 &&
                      hp2r_prepare_commands[1].data_len == 1 &&
                      hp2r_prepare_commands[1].data[0] == 0x00 &&
                      hp2r_prepare_commands[1].delay_ms == 120,
                  "initial exit sleep has a 120 ms delay");
}

static int test_command_bank_disable_precedes_display_on(void)
{
    size_t index;
    size_t bank_disable = hp2r_prepare_command_count;
    size_t display_on = hp2r_prepare_command_count;

    for (index = 0; index < hp2r_prepare_command_count; index++) {
        const struct hp2r_command *command = &hp2r_prepare_commands[index];

        if (command->command == 0xff && command->data_len == 5 &&
            command->data[0] == 0x77 && command->data[1] == 0x01 &&
            command->data[2] == 0x00 && command->data[3] == 0x00 &&
            command->data[4] == 0x00)
            bank_disable = index;
        if (command->command == 0x29)
            display_on = index;
    }

    return expect(bank_disable < display_on,
                  "command-bank disable precedes display-on");
}

static int test_display_off_precedes_enter_sleep(void)
{
    return expect(hp2r_display_off_command.command == 0x28 &&
                      hp2r_display_off_command.data_len == 1 &&
                      hp2r_display_off_command.data[0] == 0x00 &&
                      hp2r_display_off_command.delay_ms == 0 &&
                      hp2r_sleep_command.command == 0x10 &&
                      hp2r_sleep_command.data_len == 1 &&
                      hp2r_sleep_command.data[0] == 0x00 &&
                      hp2r_sleep_command.delay_ms == 120,
                  "display-off precedes enter-sleep");
}

static int test_every_command_has_at_most_16_data_bytes(void)
{
    size_t index;

    for (index = 0; index < hp2r_prepare_command_count; index++) {
        if (expect(hp2r_prepare_commands[index].data_len <= HP2R_MAX_DATA,
                   "prepare command data length is bounded"))
            return 1;
    }

    return expect(hp2r_display_off_command.data_len <= HP2R_MAX_DATA &&
                      hp2r_sleep_command.data_len <= HP2R_MAX_DATA,
                  "power-down command data length is bounded");
}

static int test_exact_480x480_timing_constants(void)
{
    return expect(HP2R_WIDTH == 480 && HP2R_HEIGHT == 480 &&
                      HP2R_CLOCK_KHZ == 19200 && HP2R_HSYNC_START == 490 &&
                      HP2R_HSYNC_END == 506 && HP2R_HTOTAL == 562 &&
                      HP2R_VSYNC_START == 495 && HP2R_VSYNC_END == 555 &&
                      HP2R_VTOTAL == 570,
                  "480x480 timing constants match the HyperPixel mode");
}

int main(void)
{
    if (test_prepare_sequence_is_byte_exact() ||
        test_null_command_and_sink_are_rejected() ||
        test_oversize_data_is_rejected_without_invoking_sink() ||
        test_sink_errors_propagate_and_stop_immediately() ||
        test_command_0x11_becomes_0x011() ||
        test_data_0x77_becomes_0x177() ||
        test_all_nine_bits_are_most_significant_bit_first() ||
        test_first_prepare_command_is_soft_reset_with_5ms_delay() ||
        test_exit_sleep_has_120ms_delay() ||
        test_command_bank_disable_precedes_display_on() ||
        test_display_off_precedes_enter_sleep() ||
        test_every_command_has_at_most_16_data_bytes() ||
        test_exact_480x480_timing_constants())
        return 1;

    puts("protocol tests passed");
    return 0;
}
