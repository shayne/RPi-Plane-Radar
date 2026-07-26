// SPDX-License-Identifier: GPL-2.0-only
#include "../planeradar_hyperpixel2r_gpio.h"

#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

struct fake_gpio {
	char events[256];
	size_t event_count;
	unsigned int delays;
	bool fail_sda;
	bool fail_scl_low;
	bool fail_cs_low;
	bool fail_release_sda;
	bool fail_release_scl;
	bool fail_backlight;
};

static int record_event(struct fake_gpio *gpio, char signal, int value)
{
	assert(gpio->event_count + 2 < sizeof(gpio->events));
	gpio->events[gpio->event_count++] = signal;
	gpio->events[gpio->event_count++] = value ? '1' : '0';
	gpio->events[gpio->event_count] = '\0';

	return 0;
}

static int fake_set_sda(void *context, int value)
{
	struct fake_gpio *gpio = context;

	record_event(gpio, 'D', value);
	if (gpio->fail_sda) {
		gpio->fail_sda = false;
		return -EIO;
	}

	return 0;
}

static int fake_set_scl(void *context, int value)
{
	struct fake_gpio *gpio = context;

	record_event(gpio, 'L', value);
	if (!value && gpio->fail_scl_low) {
		gpio->fail_scl_low = false;
		return -ERANGE;
	}

	return 0;
}

static int fake_set_cs(void *context, int value)
{
	struct fake_gpio *gpio = context;

	record_event(gpio, 'C', value);
	if (!value && gpio->fail_cs_low) {
		gpio->fail_cs_low = false;
		return -ENODEV;
	}

	return 0;
}

static int fake_release_sda(void *context)
{
	struct fake_gpio *gpio = context;

	record_event(gpio, 'S', 1);
	return gpio->fail_release_sda ? -EADDRINUSE : 0;
}

static int fake_release_scl(void *context)
{
	struct fake_gpio *gpio = context;

	record_event(gpio, 'K', 1);
	return gpio->fail_release_scl ? -EBUSY : 0;
}

static int fake_disable_backlight(void *context)
{
	struct fake_gpio *gpio = context;

	record_event(gpio, 'B', 0);
	return gpio->fail_backlight ? -EACCES : 0;
}

static void fake_delay(void *context, unsigned int delay_us)
{
	struct fake_gpio *gpio = context;

	assert(delay_us == 5);
	gpio->delays++;
}

static struct hp2r_gpio_ops fake_ops(struct fake_gpio *gpio)
{
	return (struct hp2r_gpio_ops) {
		.context = gpio,
		.set_sda = fake_set_sda,
		.set_scl = fake_set_scl,
		.set_cs = fake_set_cs,
		.release_sda = fake_release_sda,
		.release_scl = fake_release_scl,
		.disable_backlight = fake_disable_backlight,
		.delay_us = fake_delay,
	};
}

static void write_word_reports_failed_final_cs_deassertion(void)
{
	struct fake_gpio gpio = { .fail_cs_low = true };
	struct hp2r_gpio_ops ops = fake_ops(&gpio);

	assert(hp2r_gpio_write_word(&ops, 0x155) == -ENODEV);
	assert(gpio.delays == 18);
	assert(strstr(gpio.events, "C0") != NULL);
}

static void write_word_preserves_first_error_and_attempts_safe_cleanup(void)
{
	struct fake_gpio gpio = {
		.fail_sda = true,
		.fail_scl_low = true,
		.fail_cs_low = true,
	};
	struct hp2r_gpio_ops ops = fake_ops(&gpio);

	assert(hp2r_gpio_write_word(&ops, 0x100) == -EIO);
	assert(strcmp(gpio.events, "C1D1L0C0") == 0);
}

static void quiesce_attempts_every_owned_line_and_preserves_first_error(void)
{
	struct fake_gpio gpio = {
		.fail_cs_low = true,
		.fail_release_sda = true,
		.fail_release_scl = true,
		.fail_backlight = true,
	};
	struct hp2r_gpio_ops ops = fake_ops(&gpio);

	assert(hp2r_gpio_quiesce(&ops, -ENOMEM) == -ENOMEM);
	assert(strcmp(gpio.events, "C0S1K1B0") == 0);
}

static void quiesce_returns_first_cleanup_error_without_skipping_later_lines(void)
{
	struct fake_gpio gpio = {
		.fail_cs_low = true,
		.fail_release_sda = true,
	};
	struct hp2r_gpio_ops ops = fake_ops(&gpio);

	assert(hp2r_gpio_quiesce(&ops, 0) == -ENODEV);
	assert(strcmp(gpio.events, "C0S1K1B0") == 0);
}

static void quiesce_handles_partial_acquisition(void)
{
	struct fake_gpio gpio = { 0 };
	struct hp2r_gpio_ops ops = {
		.context = &gpio,
		.release_sda = fake_release_sda,
	};

	assert(hp2r_gpio_quiesce(&ops, -EAGAIN) == -EAGAIN);
	assert(strcmp(gpio.events, "S1") == 0);
}

int main(void)
{
	write_word_reports_failed_final_cs_deassertion();
	write_word_preserves_first_error_and_attempts_safe_cleanup();
	quiesce_attempts_every_owned_line_and_preserves_first_error();
	quiesce_returns_first_cleanup_error_without_skipping_later_lines();
	quiesce_handles_partial_acquisition();
	puts("GPIO safety tests passed");

	return 0;
}
