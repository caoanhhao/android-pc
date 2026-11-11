// SPDX-License-Identifier: GPL-2.0-only
/*
 * pchvc: placeholder /dev/hvc0..hvc17 for Cuttlefish's vendor HALs on the real PC.
 *
 * On Cuttlefish these are virtio-console ports to daemons on the host (oemlock, UWB,
 * confirmation UI, Bluetooth, keymint, ...). The PC has no host behind them, and HALs that
 * cannot open their port abort, which leaves system_server waiting for them forever. Here
 * every port opens like a tty, writes are discarded and reads wait for data that never
 * comes, so those HALs start and register (as on the project's Rust kernel, phase E3).
 *
 * The module sits in the same vendor_dlkm as Cuttlefish's virtio_console, so it does
 * nothing unless the kernel command line has pchvc.enable=1 (tools/mkusb-aosp17.sh).
 */

#include <linux/module.h>
#include <linux/tty.h>
#include <linux/tty_driver.h>
#include <linux/tty_flip.h>

#define PCHVC_PORTS	18
#define PCHVC_MAJOR	229	/* HVC_MAJOR, the numbers Cuttlefish's ports have */

static bool enable;
module_param(enable, bool, 0444);
MODULE_PARM_DESC(enable, "Create /dev/hvc0..17 (off: the module does nothing)");

static struct tty_driver *pchvc_driver;
static struct tty_port pchvc_ports[PCHVC_PORTS];
static const struct tty_port_operations pchvc_port_ops;

static int pchvc_open(struct tty_struct *tty, struct file *filp)
{
	return tty_port_open(&pchvc_ports[tty->index], tty, filp);
}

static void pchvc_close(struct tty_struct *tty, struct file *filp)
{
	tty_port_close(&pchvc_ports[tty->index], tty, filp);
}

static ssize_t pchvc_write(struct tty_struct *tty, const u8 *buf, size_t count)
{
	return count;
}

static unsigned int pchvc_write_room(struct tty_struct *tty)
{
	return 4096;
}

static const struct tty_operations pchvc_ops = {
	.open = pchvc_open,
	.close = pchvc_close,
	.write = pchvc_write,
	.write_room = pchvc_write_room,
};

static int __init pchvc_init(void)
{
	struct tty_driver *drv;
	int i, ret;

	if (!enable)
		return 0;

	drv = tty_alloc_driver(PCHVC_PORTS, TTY_DRIVER_REAL_RAW | TTY_DRIVER_DYNAMIC_DEV);
	if (IS_ERR(drv))
		return PTR_ERR(drv);
	drv->driver_name = "pchvc";
	drv->name = "hvc";
	drv->major = PCHVC_MAJOR;
	drv->minor_start = 0;
	drv->type = TTY_DRIVER_TYPE_SYSTEM;
	drv->init_termios = tty_std_termios;
	tty_set_operations(drv, &pchvc_ops);
	for (i = 0; i < PCHVC_PORTS; i++) {
		tty_port_init(&pchvc_ports[i]);
		pchvc_ports[i].ops = &pchvc_port_ops;
	}

	ret = tty_register_driver(drv);
	if (ret)
		goto err_ports;
	for (i = 0; i < PCHVC_PORTS; i++) {
		struct device *dev = tty_port_register_device(&pchvc_ports[i], drv, i, NULL);

		if (IS_ERR(dev)) {
			ret = PTR_ERR(dev);
			while (--i >= 0)
				tty_unregister_device(drv, i);
			tty_unregister_driver(drv);
			goto err_ports;
		}
	}
	pchvc_driver = drv;
	pr_info("pchvc: /dev/hvc0..%d without a host\n", PCHVC_PORTS - 1);
	return 0;

err_ports:
	for (i = 0; i < PCHVC_PORTS; i++)
		tty_port_destroy(&pchvc_ports[i]);
	tty_driver_kref_put(drv);
	return ret;
}

static void __exit pchvc_exit(void)
{
	int i;

	if (!pchvc_driver)
		return;
	for (i = 0; i < PCHVC_PORTS; i++)
		tty_unregister_device(pchvc_driver, i);
	tty_unregister_driver(pchvc_driver);
	for (i = 0; i < PCHVC_PORTS; i++)
		tty_port_destroy(&pchvc_ports[i]);
	tty_driver_kref_put(pchvc_driver);
}

module_init(pchvc_init);
module_exit(pchvc_exit);
MODULE_DESCRIPTION("Placeholder hvc ports for Cuttlefish HALs on a real PC");
MODULE_LICENSE("GPL");
