# Bluetooth HAL over the kernel's hci0 (HCI user channel). Cuttlefish keeps its own HAL
# (bt_hci, H4 to rootcanal over /dev/hvc5); init.pc.rc swaps it for this one on the PC.
PRODUCT_PACKAGES += android.hardware.bluetooth-service.pc
