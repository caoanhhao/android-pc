# GTS7L_HALS: the names in hals.list, read by aosp_gts7l.mk (packages) and BoardConfig.mk
# (sepolicy), which Make evaluates separately.
GTS7L_HALS := $(strip $(shell sed -e 's/\#.*//' $(PC_DEVICE_DIR)/gts7l/hals.list))
