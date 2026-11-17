#!/vendor/bin/sh
# Phase L4 (docs/nvidia-native.md): Where Winds Meet on its Vulkan renderer. The game's launcher
# script picks Vulkan when its LocalData/vulkan.tag exists (an empty file) and VulkanBroken.tag
# does not, and ES3 otherwise. Started by init.nvgpu.rc once the boot completes on the NVIDIA
# entry, this creates that empty tag in the game's data directory if the directory exists (the
# game has run once) and the tag is missing; it changes none of the game's own files. A
# VulkanBroken.tag (the game gave up on Vulkan after failed starts) is left alone: deleting it
# lets the game try Vulkan again. persist.vendor.android_pc.wwm_vulkan=0 turns this off (then
# delete the tag to go back to ES3).
export PATH=/system/bin:/vendor/bin:$PATH
k() { echo "pcwwmvk: $*" > /dev/kmsg; }

[ "$(getprop persist.vendor.android_pc.wwm_vulkan)" = 0 ] && { k "off"; exit 0; }
d=/data/media/0/Android/data/com.netease.yyvn/files/LocalData
if [ ! -d $d ]; then
    k "no $d yet (start the game once, then reboot)"
    exit 0
fi
[ -e $d/VulkanBroken.tag ] && k "$d/VulkanBroken.tag exists: the game stays on ES3 until it is deleted"
if [ ! -e $d/vulkan.tag ]; then
    touch $d/vulkan.tag && chown "$(stat -c %u:%g $d)" $d/vulkan.tag && chmod 660 $d/vulkan.tag &&
        k "created $d/vulkan.tag" || k "could not create $d/vulkan.tag"
fi
