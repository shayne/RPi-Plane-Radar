// SPDX-License-Identifier: GPL-2.0-only
#ifndef PLANERADAR_HYPERPIXEL2R_GPIO_H
#define PLANERADAR_HYPERPIXEL2R_GPIO_H

#ifdef __KERNEL__
#include <linux/types.h>
typedef u16 hp2r_gpio_word;
#else
#include <stdint.h>
typedef uint16_t hp2r_gpio_word;
#endif

struct hp2r_gpio_ops {
	void *context;
	int (*set_sda)(void *context, int value);
	int (*set_scl)(void *context, int value);
	int (*set_cs)(void *context, int value);
	int (*release_sda)(void *context);
	int (*release_scl)(void *context);
	int (*disable_backlight)(void *context);
	void (*delay_us)(void *context, unsigned int delay_us);
};

int hp2r_gpio_write_word(const struct hp2r_gpio_ops *ops,
			 hp2r_gpio_word word);
int hp2r_gpio_quiesce(const struct hp2r_gpio_ops *ops, int first_error);

#endif
