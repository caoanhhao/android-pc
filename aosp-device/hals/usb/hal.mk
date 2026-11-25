# USB HAL: IUsb (Type-C port status from the typec class) and IUsbGadget (functions on the
# configfs gadget g1). The device's init script builds the gadget and its FunctionFS
# mounts (adb, mtp, ptp) and sets sys.usb.configfs=2 on vendor.usb.gadget.hal=1
# (gts7l/init.gts7l.usb.rc).
PRODUCT_PACKAGES += android.hardware.usb-service.pc
