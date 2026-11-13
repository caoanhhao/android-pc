package pc.test.wvintercept;

import android.app.Activity;
import android.content.pm.PackageInfo;
import android.os.Bundle;
import android.util.Log;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebView;
import android.webkit.WebViewClient;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.FilterInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.zip.GZIPInputStream;
import java.util.zip.GZIPOutputStream;

/**
 * Serves a page and its scripts from shouldInterceptRequest the way TikTok's Spark pages do
 * (F9p): each script is a GZIPInputStream inside FilterInputStream wrappers, handed to
 * WebView, which calls available() and read() on its own threads. "closed.js" is the same
 * stream closed before it is returned, which gives WebView's "Stream closed" error.
 * The result is logged under the tag WVIntercept: "RESULT gz=ok big=ok closed=failed" when
 * WebView behaves as it should.
 */
public class MainActivity extends Activity {
    private static final String TAG = "WVIntercept";
    private static final String BASE = "https://wvintercept.test/";

    private static final String PAGE = "<html><head>"
            + "<script>var r = {gz: 'failed', big: 'failed', closed: 'failed'};</script>"
            + "<script src='gz.js'></script>"
            + "<script src='big.js'></script>"
            + "<script src='closed.js'></script>"
            + "</head><body><script>"
            + "document.title = 'RESULT gz=' + r.gz + ' big=' + r.big + ' closed=' + r.closed;"
            + "</script></body></html>";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        PackageInfo provider = WebView.getCurrentWebViewPackage();
        Log.i(TAG, "provider " + (provider == null ? "none"
                : provider.packageName + " " + provider.versionName));
        WebView view = new WebView(this);
        view.getSettings().setJavaScriptEnabled(true);
        view.setWebViewClient(new WebViewClient() {
            @Override
            public WebResourceResponse shouldInterceptRequest(WebView v, WebResourceRequest req) {
                return intercept(req.getUrl().toString());
            }

            @Override
            public void onPageFinished(WebView v, String url) {
                Log.i(TAG, v.getTitle());
            }
        });
        setContentView(view);
        view.loadUrl(BASE + "index.html");
    }

    private static WebResourceResponse intercept(String url) {
        try {
            switch (url.substring(url.lastIndexOf('/') + 1)) {
                case "index.html":
                    return new WebResourceResponse("text/html", "utf-8", wrap(gzip(PAGE)));
                case "gz.js":
                    return new WebResourceResponse("text/javascript", "utf-8",
                            wrap(gzip("r.gz = 'ok';")));
                case "big.js": {
                    // Larger than WebView's read buffer, so read() is called many times.
                    StringBuilder js = new StringBuilder("var pad = [");
                    for (int i = 0; i < 100000; i++) js.append(i).append(',');
                    js.append("0]; r.big = pad.length == 100001 ? 'ok' : 'short';");
                    return new WebResourceResponse("text/javascript", "utf-8",
                            wrap(gzip(js.toString())));
                }
                case "closed.js": {
                    InputStream in = wrap(gzip("r.closed = 'read';"));
                    in.close();
                    return new WebResourceResponse("text/javascript", "utf-8", in);
                }
                default:
                    return null;
            }
        } catch (IOException e) {
            Log.e(TAG, "intercept " + url, e);
            return null;
        }
    }

    private static InputStream wrap(byte[] gz) throws IOException {
        InputStream in = new GZIPInputStream(new ByteArrayInputStream(gz));
        return new FilterInputStream(new FilterInputStream(in) {}) {};
    }

    private static byte[] gzip(String s) throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        try (GZIPOutputStream gz = new GZIPOutputStream(out)) {
            gz.write(s.getBytes(StandardCharsets.UTF_8));
        }
        return out.toByteArray();
    }
}
