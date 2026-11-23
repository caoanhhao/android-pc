# Primary audio module in Rust (IModule/default over ALSA). AOSP's default audio HAL APEX
# (com.android.hardware.audio) stays for IConfig/default, the r_submix and bluetooth modules
# and the effects. It creates one module per <module> of
# /vendor/etc/audio_policy_configuration.xml, so ours has no "primary" module.
PRODUCT_PACKAGES += android.hardware.audio.service.pc
$(call inherit-product, hardware/interfaces/audio/aidl/default/audio_effects.mk)
