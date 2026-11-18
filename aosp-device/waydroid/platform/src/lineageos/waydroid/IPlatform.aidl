// Waydroid's platform service, as the host side calls it (/usr/lib/waydroid/tools/interfaces/
// IPlatform.py): the descriptor and the transaction codes (declaration order, from 1) must match.
package lineageos.waydroid;

import lineageos.waydroid.AppInfo;

interface IPlatform {
    String getprop(String arg1, String arg2);
    void setprop(String arg1, String arg2);
    List<AppInfo> getAppsInfo();
    AppInfo getAppInfo(String arg1);
    int installApp(String arg1);
    int removeApp(String arg1);
    void launchApp(String arg1);
    String getAppName(String arg1);
    void settingsPutString(int mode, String key, String value);
    String settingsGetString(int mode, String key);
    void settingsPutInt(int mode, String key, int value);
    int settingsGetInt(int mode, String key);
    String launchIntent(String action, String uri);
}
