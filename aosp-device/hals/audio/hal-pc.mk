# PC product: setting LOCAL_AUDIO_PRODUCT_COPY_FILES makes Cuttlefish install our policy
# configuration instead of its own; the effects configuration Cuttlefish adds along with its
# own comes from hal.mk.
LOCAL_AUDIO_PRODUCT_COPY_FILES = \
    $(PC_DEVICE_DIR)/hals/audio/audio_policy_configuration.xml:$(TARGET_COPY_OUT_VENDOR)/etc/audio_policy_configuration.xml
# HDMI audio: the framework connects the HAL's HDMI device port when the HD Audio driver
# reports a monitor on an HDMI/DP jack (input switches, see overlay/).
DEVICE_PACKAGE_OVERLAYS += $(PC_DEVICE_DIR)/hals/audio/overlay
