# Plane Radar HyperPixel 2 Round protocol

`planeradar_hyperpixel2r_protocol.c` contains an explicit, fixed-mode
translation of the HyperPixel 2 Round ST7701 command stream. It is shared by
the kernel module and a host-side protocol test without depending on kernel
headers in the test build.

The command values are derived from Raspberry Pi Linux commit
[`33bb14b06b3fb5a682d4a7a3db3963fe558fc6f9`](https://github.com/raspberrypi/linux/blob/33bb14b06b3fb5a682d4a7a3db3963fe558fc6f9/drivers/gpu/drm/panel/panel-sitronix-st7701.c),
source path `drivers/gpu/drm/panel/panel-sitronix-st7701.c`. They translate
`st7701_init_sequence`, `txw210001b0_gip_sequence`,
`hyperpixel2r_desc.pv_gamma`, `hyperpixel2r_desc.nv_gamma`, and the common
display-on/off commands for the 480 by 480 HyperPixel mode.

The source driver carries these notices, which are preserved here:

```
Copyright (C) 2019, Amarula Solutions.
Author: Jagan Teki <jagan@amarulasolutions.com>
```

The protocol source is licensed under GPL-2.0-only. The canonical GNU GPL
version 2 text is in `LICENSE`.
