package com.minarbot.mobile

import com.google.firebase.messaging.FirebaseMessagingService

/** Passes a rotated FCM token to the app; Android shows notifications itself while the app is in the background. */
class PushService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        NativePlugin.tokenRefreshed(token)
    }
}
