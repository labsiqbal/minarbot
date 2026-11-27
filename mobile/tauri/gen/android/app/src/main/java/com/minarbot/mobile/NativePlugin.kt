package com.minarbot.mobile

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.util.Log
import android.webkit.WebView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging

@InvokeArg
class SystemBarsArgs {
    var light: Boolean = true
    var color: Int = 0
}

/**
 * Minarbot's Android code. Rust calls `register` and `systemBars` and listens for
 * `tap`, `token` and `resume`; no webview has a permission for it.
 *
 * Without google-services.json the build has no Firebase app, so `register`
 * reports the reason instead of a token and the app works without push.
 */
@TauriPlugin
class NativePlugin(private val activity: Activity) : Plugin(activity) {
    private var opened: String? = null

    override fun load(webView: WebView) {
        instance = this
        opened = route(activity.intent)
    }

    /** The owner came back to the app; Rust checks the server. */
    override fun onRestart(activity: AppCompatActivity) {
        trigger("resume", JSObject())
    }

    override fun onNewIntent(intent: Intent) {
        val path = route(intent) ?: return
        val event = JSObject()
        event.put("path", path)
        trigger("tap", event)
    }

    @Command
    fun register(invoke: Invoke) {
        val reply = JSObject()
        reply.put("name", Build.MODEL)
        opened?.let {
            reply.put("opened", it)
            opened = null
        }
        if (!configured()) {
            reply.put("reason", "This build has no Firebase config (google-services.json).")
            invoke.resolve(reply)
            return
        }
        askToNotify()
        FirebaseMessaging.getInstance().token
            .addOnSuccessListener { token ->
                reply.put("token", token)
                invoke.resolve(reply)
            }
            .addOnFailureListener { error ->
                reply.put("reason", "Firebase gave no token: ${error.message}")
                invoke.resolve(reply)
            }
    }

    /** Paints the bars with the page's top colour, with icons that contrast with it. */
    @Command
    fun systemBars(invoke: Invoke) {
        val args = invoke.parseArgs(SystemBarsArgs::class.java)
        activity.runOnUiThread {
            val bars = WindowCompat.getInsetsController(activity.window, activity.window.decorView)
            bars.isAppearanceLightStatusBars = args.light
            bars.isAppearanceLightNavigationBars = args.light
            activity.window.decorView.setBackgroundColor(args.color)
            invoke.resolve()
        }
    }

    private fun configured(): Boolean =
        FirebaseApp.getApps(activity).isNotEmpty() || FirebaseApp.initializeApp(activity) != null

    private fun askToNotify() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        val permission = Manifest.permission.POST_NOTIFICATIONS
        if (ContextCompat.checkSelfPermission(activity, permission) != PackageManager.PERMISSION_GRANTED) {
            ActivityCompat.requestPermissions(activity, arrayOf(permission), 1709)
        }
    }

    companion object {
        @Volatile
        private var instance: NativePlugin? = null

        /** The route of a notification the owner tapped, when the intent came from FCM. */
        fun route(intent: Intent?): String? {
            val extras = intent?.extras ?: return null
            if (!extras.containsKey("google.message_id")) return null
            return extras.getString("path")
        }

        fun tokenRefreshed(token: String) {
            val plugin = instance
            if (plugin == null) {
                Log.i("MinarbotPush", "Token rotated before the app loaded; it registers on next launch.")
                return
            }
            val event = JSObject()
            event.put("token", token)
            event.put("name", Build.MODEL)
            plugin.trigger("token", event)
        }
    }
}
