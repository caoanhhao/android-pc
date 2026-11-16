// NVIDIA's instance-level functions the HAL calls itself (resolved once from the first instance;
// NVIDIA returns the same entry points for every instance).
NVVK_FN(DestroyInstance)
NVVK_FN(EnumeratePhysicalDevices)
NVVK_FN(EnumerateDeviceExtensionProperties)
NVVK_FN(GetPhysicalDeviceProperties)
NVVK_FN(GetPhysicalDeviceProperties2)
NVVK_FN(GetPhysicalDeviceMemoryProperties)
NVVK_FN(GetPhysicalDeviceFormatProperties2)
NVVK_FN(GetPhysicalDeviceImageFormatProperties2)
NVVK_FN(GetPhysicalDeviceExternalBufferProperties)
NVVK_FN(CreateDevice)
NVVK_FN(GetDeviceProcAddr)
