// SPDX-License-Identifier: GPL-2.0-only
/*
 * Plane Radar HyperPixel 2.1 Round display and shared touch bus.
 *
 * Kernel API provenance:
 * raspberrypi/linux commit 33bb14b06b3fb5a682d4a7a3db3963fe558fc6f9
 */

#include <linux/delay.h>
#include <linux/device.h>
#include <linux/gpio/consumer.h>
#include <linux/i2c-algo-bit.h>
#include <linux/i2c.h>
#include <linux/media-bus-format.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/platform_device.h>

#include <drm/drm_connector.h>
#include <drm/drm_modes.h>
#include <drm/drm_panel.h>

#include "planeradar_hyperpixel2r_gpio.h"
#include "planeradar_hyperpixel2r_protocol.h"

#ifndef GPIOD_OUT_INACTIVE
#define GPIOD_OUT_INACTIVE GPIOD_OUT_LOW
#endif

struct planeradar_hyperpixel2r {
	struct device *dev;
	struct drm_panel panel;
	struct gpio_desc *sda;
	struct gpio_desc *scl;
	struct gpio_desc *cs;
	struct gpio_desc *backlight;
	struct i2c_adapter adapter;
	struct i2c_algo_bit_data bit;
	struct mutex state_lock;
	enum drm_panel_orientation orientation;
	bool prepared;
	bool enabled;
};

static inline struct planeradar_hyperpixel2r *
panel_to_hyperpixel2r(struct drm_panel *panel)
{
	return container_of(panel, struct planeradar_hyperpixel2r, panel);
}

static void hp2r_setsda(void *context, int high)
{
	struct planeradar_hyperpixel2r *hp = context;

	if (high)
		gpiod_direction_input(hp->sda);
	else
		gpiod_direction_output(hp->sda, 0);
}

static void hp2r_setscl(void *context, int high)
{
	struct planeradar_hyperpixel2r *hp = context;

	if (high)
		gpiod_direction_input(hp->scl);
	else
		gpiod_direction_output(hp->scl, 0);
}

static int hp2r_getsda(void *context)
{
	struct planeradar_hyperpixel2r *hp = context;

	return gpiod_get_value(hp->sda);
}

static int hp2r_getscl(void *context)
{
	struct planeradar_hyperpixel2r *hp = context;

	return gpiod_get_value(hp->scl);
}

static int hp2r_release_i2c_lines(struct planeradar_hyperpixel2r *hp)
{
	int ret;
	int scl_ret;

	ret = gpiod_direction_input(hp->sda);
	scl_ret = gpiod_direction_input(hp->scl);

	return ret ? ret : scl_ret;
}

static int hp2r_set_sda_value(void *context, int value)
{
	struct planeradar_hyperpixel2r *hp = context;

	return gpiod_set_value(hp->sda, value);
}

static int hp2r_set_scl_value(void *context, int value)
{
	struct planeradar_hyperpixel2r *hp = context;

	return gpiod_set_value(hp->scl, value);
}

static int hp2r_set_cs_value(void *context, int value)
{
	struct planeradar_hyperpixel2r *hp = context;

	return gpiod_set_value(hp->cs, value);
}

static int hp2r_release_sda(void *context)
{
	struct planeradar_hyperpixel2r *hp = context;

	return gpiod_direction_input(hp->sda);
}

static int hp2r_release_scl(void *context)
{
	struct planeradar_hyperpixel2r *hp = context;

	return gpiod_direction_input(hp->scl);
}

static int hp2r_disable_backlight(void *context)
{
	struct planeradar_hyperpixel2r *hp = context;

	return gpiod_direction_output(hp->backlight, 0);
}

static void hp2r_delay_us(void *context, unsigned int delay_us)
{
	(void)context;
	udelay(delay_us);
}

static struct hp2r_gpio_ops
hp2r_gpio_ops(struct planeradar_hyperpixel2r *hp, bool include_backlight)
{
	return (struct hp2r_gpio_ops) {
		.context = hp,
		.set_sda = hp->sda ? hp2r_set_sda_value : NULL,
		.set_scl = hp->scl ? hp2r_set_scl_value : NULL,
		.set_cs = hp->cs ? hp2r_set_cs_value : NULL,
		.release_sda = hp->sda ? hp2r_release_sda : NULL,
		.release_scl = hp->scl ? hp2r_release_scl : NULL,
		.disable_backlight =
			include_backlight && hp->backlight ?
				hp2r_disable_backlight : NULL,
		.delay_us = hp2r_delay_us,
	};
}

static int hp2r_write_word(void *context, hp2r_u16 word)
{
	struct planeradar_hyperpixel2r *hp = context;
	struct hp2r_gpio_ops ops = hp2r_gpio_ops(hp, false);

	return hp2r_gpio_write_word(&ops, word);
}

static int
hp2r_send_commands(struct planeradar_hyperpixel2r *hp,
		   const struct hp2r_command *commands, size_t command_count)
{
	struct hp2r_gpio_ops ops = hp2r_gpio_ops(hp, false);
	size_t index;
	int ret = 0;

	i2c_lock_bus(&hp->adapter, I2C_LOCK_ROOT_ADAPTER);
	mutex_lock(&hp->state_lock);

	ret = gpiod_direction_output(hp->scl, 0);
	if (ret)
		goto restore_i2c;
	ret = gpiod_direction_output(hp->sda, 0);
	if (ret)
		goto restore_i2c;

	for (index = 0; index < command_count; index++) {
		ret = hp2r_emit_command(&commands[index], hp2r_write_word, hp);
		if (ret)
			goto restore_i2c;
		if (commands[index].delay_ms)
			msleep(commands[index].delay_ms);
	}

restore_i2c:
	ret = hp2r_gpio_quiesce(&ops, ret);
	mutex_unlock(&hp->state_lock);
	i2c_unlock_bus(&hp->adapter, I2C_LOCK_ROOT_ADAPTER);

	return ret;
}

static int hp2r_panel_prepare(struct drm_panel *panel)
{
	struct planeradar_hyperpixel2r *hp = panel_to_hyperpixel2r(panel);
	int ret = 0;

	mutex_lock(&hp->state_lock);
	if (hp->prepared)
		goto unlock;
	mutex_unlock(&hp->state_lock);

	ret = hp2r_send_commands(hp, hp2r_prepare_commands,
				 hp2r_prepare_command_count);
	if (ret)
		return ret;

	mutex_lock(&hp->state_lock);
	hp->prepared = true;
unlock:
	mutex_unlock(&hp->state_lock);

	return ret;
}

static int hp2r_panel_enable(struct drm_panel *panel)
{
	struct planeradar_hyperpixel2r *hp = panel_to_hyperpixel2r(panel);
	int ret = 0;

	mutex_lock(&hp->state_lock);
	if (hp->enabled)
		goto unlock;
	if (!hp->prepared) {
		ret = -EPERM;
		goto unlock;
	}

	ret = gpiod_direction_output(hp->backlight, 1);
	if (!ret)
		hp->enabled = true;

unlock:
	mutex_unlock(&hp->state_lock);

	return ret;
}

static int hp2r_panel_disable(struct drm_panel *panel)
{
	struct planeradar_hyperpixel2r *hp = panel_to_hyperpixel2r(panel);
	int ret;

	mutex_lock(&hp->state_lock);
	ret = gpiod_direction_output(hp->backlight, 0);
	hp->enabled = false;
	mutex_unlock(&hp->state_lock);

	return ret;
}

static int hp2r_panel_unprepare(struct drm_panel *panel)
{
	struct planeradar_hyperpixel2r *hp = panel_to_hyperpixel2r(panel);
	struct hp2r_command commands[] = {
		hp2r_display_off_command,
		hp2r_sleep_command,
	};
	int backlight_ret;
	int ret = 0;

	mutex_lock(&hp->state_lock);
	if (!hp->prepared)
		goto unlock;
	backlight_ret = gpiod_direction_output(hp->backlight, 0);
	hp->enabled = false;
	mutex_unlock(&hp->state_lock);

	ret = hp2r_send_commands(hp, commands, ARRAY_SIZE(commands));
	if (!ret)
		ret = backlight_ret;

	mutex_lock(&hp->state_lock);
	hp->prepared = false;
unlock:
	mutex_unlock(&hp->state_lock);

	return ret;
}

static int hp2r_panel_get_modes(struct drm_panel *panel,
				struct drm_connector *connector)
{
	static const struct drm_display_mode default_mode = {
		.clock = HP2R_CLOCK_KHZ,
		.hdisplay = HP2R_WIDTH,
		.hsync_start = HP2R_HSYNC_START,
		.hsync_end = HP2R_HSYNC_END,
		.htotal = HP2R_HTOTAL,
		.vdisplay = HP2R_HEIGHT,
		.vsync_start = HP2R_VSYNC_START,
		.vsync_end = HP2R_VSYNC_END,
		.vtotal = HP2R_VTOTAL,
		.flags = DRM_MODE_FLAG_NHSYNC | DRM_MODE_FLAG_NVSYNC,
	};
	static const u32 bus_formats[] = {
		MEDIA_BUS_FMT_RGB666_1X18,
	};
	struct drm_display_mode *mode;
	int ret;

	ret = drm_display_info_set_bus_formats(&connector->display_info,
					       bus_formats,
					       ARRAY_SIZE(bus_formats));
	if (ret)
		return ret;

	mode = drm_mode_duplicate(connector->dev, &default_mode);
	if (!mode)
		return -ENOMEM;

	mode->type = DRM_MODE_TYPE_DRIVER | DRM_MODE_TYPE_PREFERRED;
	drm_mode_set_name(mode);
	drm_mode_probed_add(connector, mode);

	connector->display_info.width_mm = 53;
	connector->display_info.height_mm = 53;
	connector->display_info.bus_flags =
		DRM_BUS_FLAG_PIXDATA_DRIVE_NEGEDGE;

	return 1;
}

static enum drm_panel_orientation
hp2r_panel_get_orientation(struct drm_panel *panel)
{
	struct planeradar_hyperpixel2r *hp = panel_to_hyperpixel2r(panel);

	return hp->orientation;
}

static const struct drm_panel_funcs hp2r_panel_funcs = {
	.prepare = hp2r_panel_prepare,
	.enable = hp2r_panel_enable,
	.disable = hp2r_panel_disable,
	.unprepare = hp2r_panel_unprepare,
	.get_modes = hp2r_panel_get_modes,
	.get_orientation = hp2r_panel_get_orientation,
};

static void hp2r_unregister_i2c(void *data)
{
	struct planeradar_hyperpixel2r *hp = data;

	i2c_del_adapter(&hp->adapter);
}

static int hp2r_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct planeradar_hyperpixel2r *hp;
	struct hp2r_gpio_ops gpio_ops;
	int ret;

	hp = devm_drm_panel_alloc(dev, struct planeradar_hyperpixel2r, panel,
				 &hp2r_panel_funcs, DRM_MODE_CONNECTOR_DPI);
	if (IS_ERR(hp))
		return PTR_ERR(hp);

	hp->dev = dev;
	mutex_init(&hp->state_lock);

	hp->sda = devm_gpiod_get(dev, "sda", GPIOD_ASIS);
	if (IS_ERR(hp->sda)) {
		ret = dev_err_probe(dev, PTR_ERR(hp->sda),
				    "failed to acquire SDA GPIO\n");
		hp->sda = NULL;
		goto quiesce_gpios;
	}

	hp->scl = devm_gpiod_get(dev, "scl", GPIOD_ASIS);
	if (IS_ERR(hp->scl)) {
		ret = dev_err_probe(dev, PTR_ERR(hp->scl),
				    "failed to acquire SCL GPIO\n");
		hp->scl = NULL;
		goto quiesce_gpios;
	}

	hp->cs = devm_gpiod_get(dev, "cs", GPIOD_OUT_INACTIVE);
	if (IS_ERR(hp->cs)) {
		ret = dev_err_probe(dev, PTR_ERR(hp->cs),
				    "failed to acquire chip-select GPIO\n");
		hp->cs = NULL;
		goto quiesce_gpios;
	}

	hp->backlight =
		devm_gpiod_get(dev, "backlight", GPIOD_OUT_INACTIVE);
	if (IS_ERR(hp->backlight)) {
		ret = dev_err_probe(dev, PTR_ERR(hp->backlight),
				    "failed to acquire backlight GPIO\n");
		hp->backlight = NULL;
		goto quiesce_gpios;
	}

	if (gpiod_cansleep(hp->sda) || gpiod_cansleep(hp->scl) ||
	    gpiod_cansleep(hp->cs)) {
		ret = -EINVAL;
		dev_err(dev, "SDA, SCL, and chip-select GPIOs must not sleep\n");
		goto quiesce_gpios;
	}

	ret = hp2r_release_i2c_lines(hp);
	if (ret)
		goto quiesce_gpios;

	ret = of_drm_get_panel_orientation(dev->of_node, &hp->orientation);
	if (ret)
		goto quiesce_gpios;

	hp->bit.data = hp;
	hp->bit.setsda = hp2r_setsda;
	hp->bit.setscl = hp2r_setscl;
	hp->bit.getsda = hp2r_getsda;
	hp->bit.getscl = hp2r_getscl;
	hp->bit.udelay = 4;
	hp->bit.timeout = HZ / 10;
	hp->bit.can_do_atomic = true;

	hp->adapter.owner = THIS_MODULE;
	hp->adapter.algo_data = &hp->bit;
	hp->adapter.dev.parent = hp->dev;
	device_set_node(&hp->adapter.dev, dev_fwnode(hp->dev));
	strscpy(hp->adapter.name, "planeradar-hyperpixel2r",
		sizeof(hp->adapter.name));

	ret = i2c_bit_add_bus(&hp->adapter);
	if (ret)
		goto quiesce_gpios;

	ret = devm_add_action_or_reset(dev, hp2r_unregister_i2c, hp);
	if (ret)
		goto quiesce_gpios;

	hp->panel.prepare_prev_first = true;
	drm_panel_add(&hp->panel);
	platform_set_drvdata(pdev, hp);

	return 0;

quiesce_gpios:
	gpio_ops = hp2r_gpio_ops(hp, true);
	return hp2r_gpio_quiesce(&gpio_ops, ret);
}

static void hp2r_remove(struct platform_device *pdev)
{
	struct planeradar_hyperpixel2r *hp = platform_get_drvdata(pdev);
	int ret;

	drm_panel_remove(&hp->panel);

	ret = hp2r_panel_disable(&hp->panel);
	if (ret)
		dev_warn(hp->dev, "failed to disable panel: %d\n", ret);
	ret = hp2r_panel_unprepare(&hp->panel);
	if (ret)
		dev_warn(hp->dev, "failed to unprepare panel: %d\n", ret);
}

static const struct of_device_id planeradar_hyperpixel2r_of_match[] = {
	{ .compatible = "planeradar,hyperpixel2r" },
	{ }
};
MODULE_DEVICE_TABLE(of, planeradar_hyperpixel2r_of_match);

static struct platform_driver planeradar_hyperpixel2r_driver = {
	.probe = hp2r_probe,
	.remove = hp2r_remove,
	.driver = {
		.name = "planeradar-hyperpixel2r",
		.of_match_table = planeradar_hyperpixel2r_of_match,
	},
};
module_platform_driver(planeradar_hyperpixel2r_driver);

MODULE_DESCRIPTION("Plane Radar HyperPixel 2.1 Round display and touch bus");
MODULE_LICENSE("GPL");
MODULE_SOFTDEP("pre: edt_ft5x06");
