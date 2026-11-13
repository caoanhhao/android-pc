# Primary audio module in Rust (IModule/default over ALSA). AOSP's default audio HAL APEX
# (com.android.hardware.audio, Cuttlefish's LOCAL_AUDIO_PRODUCT_PACKAGE) stays for
# IConfig/default, the r_submix and bluetooth modules and the effects. It creates one
# module per <module> of /vendor/etc/audio_policy_configuration.xml, so ours has no
# "primary" module. Setting LOCAL_AUDIO_PRODUCT_COPY_FILES makes Cuttlefish install it
# instead of its own; the effects configuration Cuttlefish adds along with its own is
# included here.
PRODUCT_PACKAGES += android.hardware.audio.service.pc
LOCAL_AUDIO_PRODUCT_COPY_FILES = \
    $(PC_DEVICE_DIR)/hals/audio/audio_policy_configuration.xml:$(TARGET_COPY_OUT_VENDOR)/etc/audio_policy_configuration.xml
$(call inherit-product, hardware/interfaces/audio/aidl/default/audio_effects.mk)
# HDMI audio: the framework connects the HAL's HDMI device port when the HD Audio driver
# reports a monitor on an HDMI/DP jack (input switches, see overlay/).
DEVICE_PACKAGE_OVERLAYS += $(PC_DEVICE_DIR)/hals/audio/overlay
