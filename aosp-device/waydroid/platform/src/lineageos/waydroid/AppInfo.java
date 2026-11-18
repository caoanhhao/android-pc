package lineageos.waydroid;

import android.os.Parcel;
import android.os.Parcelable;

import java.util.ArrayList;
import java.util.List;

/** One launchable app, in the field order the host's IPlatform.py reads. */
public final class AppInfo implements Parcelable {
    public String name;
    public String packageName;
    public String action;
    public String launchIntent;
    public String componentPackageName;
    public String componentClassName;
    public List<String> categories = new ArrayList<>();

    public AppInfo() {}

    private AppInfo(Parcel in) {
        name = in.readString();
        packageName = in.readString();
        action = in.readString();
        launchIntent = in.readString();
        componentPackageName = in.readString();
        componentClassName = in.readString();
        in.readStringList(categories);
    }

    @Override
    public void writeToParcel(Parcel out, int flags) {
        out.writeString(name);
        out.writeString(packageName);
        out.writeString(action);
        out.writeString(launchIntent);
        out.writeString(componentPackageName);
        out.writeString(componentClassName);
        out.writeStringList(categories);
    }

    @Override
    public int describeContents() {
        return 0;
    }

    public static final Creator<AppInfo> CREATOR = new Creator<AppInfo>() {
        @Override
        public AppInfo createFromParcel(Parcel in) {
            return new AppInfo(in);
        }

        @Override
        public AppInfo[] newArray(int size) {
            return new AppInfo[size];
        }
    };
}
