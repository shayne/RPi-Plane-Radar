// SPDX-License-Identifier: GPL-2.0-only
#include "planeradar_hyperpixel2r_gpio.h"

#ifdef __KERNEL__
#include <linux/errno.h>
#else
#include <errno.h>
#endif

static void hp2r_record_first_error(int *first_error, int error)
{
	if (!*first_error && error)
		*first_error = error;
}

int hp2r_gpio_write_word(const struct hp2r_gpio_ops *ops,
			 hp2r_gpio_word word)
{
	int ret = 0;
	int bit;

	if (!ops || !ops->set_sda || !ops->set_scl || !ops->set_cs ||
	    !ops->delay_us)
		return -EINVAL;

	ret = ops->set_cs(ops->context, 1);
	if (ret)
		goto deassert;

	for (bit = 8; bit >= 0; bit--) {
		ret = ops->set_sda(ops->context, word & (1U << bit));
		if (ret)
			break;
		ops->delay_us(ops->context, 5);

		ret = ops->set_scl(ops->context, 1);
		if (ret)
			break;
		ops->delay_us(ops->context, 5);

		ret = ops->set_scl(ops->context, 0);
		if (ret)
			break;
	}

deassert:
	hp2r_record_first_error(&ret, ops->set_scl(ops->context, 0));
	hp2r_record_first_error(&ret, ops->set_cs(ops->context, 0));

	return ret;
}

int hp2r_gpio_quiesce(const struct hp2r_gpio_ops *ops, int first_error)
{
	if (!ops)
		return first_error ? first_error : -EINVAL;

	if (ops->set_cs)
		hp2r_record_first_error(
			&first_error, ops->set_cs(ops->context, 0));
	if (ops->release_sda)
		hp2r_record_first_error(
			&first_error, ops->release_sda(ops->context));
	if (ops->release_scl)
		hp2r_record_first_error(
			&first_error, ops->release_scl(ops->context));
	if (ops->disable_backlight)
		hp2r_record_first_error(
			&first_error, ops->disable_backlight(ops->context));

	return first_error;
}
