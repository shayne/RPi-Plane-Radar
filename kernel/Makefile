KERNELRELEASE ?= $(shell uname -r)
KDIR ?= /lib/modules/$(KERNELRELEASE)/build

modules:
	$(MAKE) -C $(KDIR) M=$(CURDIR) W=1 modules

clean:
	$(MAKE) -C $(KDIR) M=$(CURDIR) clean
