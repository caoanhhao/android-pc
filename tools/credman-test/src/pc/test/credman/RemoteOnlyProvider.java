package pc.test.credman;

import android.app.slice.Slice;
import android.app.slice.SliceSpec;
import android.credentials.ClearCredentialStateException;
import android.credentials.CreateCredentialException;
import android.credentials.GetCredentialException;
import android.net.Uri;
import android.os.CancellationSignal;
import android.os.OutcomeReceiver;
import android.service.credentials.BeginCreateCredentialRequest;
import android.service.credentials.BeginCreateCredentialResponse;
import android.service.credentials.BeginGetCredentialRequest;
import android.service.credentials.BeginGetCredentialResponse;
import android.service.credentials.ClearCredentialStateRequest;
import android.service.credentials.CredentialProviderService;
import android.service.credentials.RemoteEntry;
import android.util.Log;

/**
 * Answers every get request with only a remote (hybrid) entry, as microG's RemoteService
 * does on the PC. The framework drops that entry because this service is not the OEM
 * hybrid service, which leaves the UI with nothing to show.
 */
public class RemoteOnlyProvider extends CredentialProviderService {
    @Override
    public void onBeginGetCredential(BeginGetCredentialRequest request,
            CancellationSignal cancellationSignal,
            OutcomeReceiver<BeginGetCredentialResponse, GetCredentialException> callback) {
        Log.i(MainActivity.TAG, "provider: answering with a remote entry only");
        Slice slice = new Slice.Builder(Uri.parse("pc-test://remote"),
                new SliceSpec("RemoteEntry", 1)).build();
        callback.onResult(new BeginGetCredentialResponse.Builder()
                .setRemoteCredentialEntry(new RemoteEntry(slice)).build());
    }

    @Override
    public void onBeginCreateCredential(BeginCreateCredentialRequest request,
            CancellationSignal cancellationSignal,
            OutcomeReceiver<BeginCreateCredentialResponse, CreateCredentialException> callback) {
        callback.onError(new CreateCredentialException(
                CreateCredentialException.TYPE_NO_CREATE_OPTIONS));
    }

    @Override
    public void onClearCredentialState(ClearCredentialStateRequest request,
            CancellationSignal cancellationSignal,
            OutcomeReceiver<Void, ClearCredentialStateException> callback) {
        callback.onResult(null);
    }
}
