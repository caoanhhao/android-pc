package pc.test.credman;

import android.app.Activity;
import android.credentials.CredentialManager;
import android.credentials.CredentialOption;
import android.credentials.GetCredentialException;
import android.credentials.GetCredentialRequest;
import android.credentials.GetCredentialResponse;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.Handler;
import android.os.Looper;
import android.os.OutcomeReceiver;
import android.util.Log;

/**
 * Asks Credential Manager for a passkey, as TikTok's identity check page does, and logs
 * how the request ends: an error type, a response, or "hang" when nothing comes back.
 */
public class MainActivity extends Activity {
    static final String TAG = "CredManTest";
    private static final long HANG_MS = 15_000;
    private boolean mDone;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        String json = "{\"challenge\":\"AAAAAAAAAAAAAAAAAAAAAA\",\"rpId\":\"example.com\","
                + "\"allowCredentials\":[],\"timeout\":60000,\"userVerification\":\"preferred\"}";
        Bundle data = new Bundle();
        data.putString("androidx.credentials.BUNDLE_KEY_SUBTYPE",
                "androidx.credentials.BUNDLE_VALUE_SUBTYPE_GET_PUBLIC_KEY_CREDENTIAL_OPTION");
        data.putString("androidx.credentials.BUNDLE_KEY_REQUEST_JSON", json);
        CredentialOption option = new CredentialOption.Builder(
                "androidx.credentials.TYPE_PUBLIC_KEY_CREDENTIAL", data, data).build();
        GetCredentialRequest request = new GetCredentialRequest.Builder(new Bundle())
                .addCredentialOption(option).build();

        Handler main = new Handler(Looper.getMainLooper());
        main.postDelayed(() -> finish("RESULT hang"), HANG_MS);
        getSystemService(CredentialManager.class).getCredential(this, request,
                new CancellationSignal(), getMainExecutor(),
                new OutcomeReceiver<GetCredentialResponse, GetCredentialException>() {
                    @Override
                    public void onResult(GetCredentialResponse response) {
                        finish("RESULT response");
                    }

                    @Override
                    public void onError(GetCredentialException e) {
                        finish("RESULT error=" + e.getType());
                    }
                });
        Log.i(TAG, "getCredential sent");
    }

    private void finish(String result) {
        if (mDone) return;
        mDone = true;
        Log.i(TAG, result);
    }
}
