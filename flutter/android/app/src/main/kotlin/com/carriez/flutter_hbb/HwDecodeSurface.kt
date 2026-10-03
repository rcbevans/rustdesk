package com.carriez.flutter_hbb

import android.util.Log
import android.view.Surface
import ffi.FFI
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.renderer.FlutterRenderer
import io.flutter.plugin.common.MethodChannel
import io.flutter.view.TextureRegistry.SurfaceTextureEntry
import java.util.concurrent.ConcurrentHashMap

// SurfaceTexture entries for zero-copy MediaCodec decoding. The flutter
// engine installs its own onFrameAvailableListener and calls updateTexImage
// on the raster thread, so this class must not notify per frame and must not
// attach or detach GL contexts.
object HwDecodeSurface {
    private const val channelTag = "mHwDecodeSurfaceChannel"
    private var renderer: FlutterRenderer? = null
    private val entries = ConcurrentHashMap<String, SurfaceTextureEntry>()

    fun register(flutterEngine: FlutterEngine) {
        renderer = flutterEngine.renderer
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, channelTag)
            .setMethodCallHandler { call, result ->
                val arguments = call.arguments as? Map<*, *>
                val sessionId = arguments?.get("sessionId") as? String
                val display = arguments?.get("display") as? Int
                if (sessionId == null || display == null) {
                    result.error("-1", "Invalid arguments", null)
                    return@setMethodCallHandler
                }
                when (call.method) {
                    "create" -> result.success(create(sessionId, display))
                    "destroy" -> {
                        destroy(sessionId, display)
                        result.success(true)
                    }
                    else -> result.error("-1", "No such method", null)
                }
            }
    }

    private fun create(sessionId: String, display: Int): Int {
        val renderer = renderer ?: return -1
        val key = "$sessionId-$display"
        if (entries.containsKey(key)) return -1
        return try {
            val entry = renderer.createSurfaceTexture()
            val surface = Surface(entry.surfaceTexture())
            val id = entry.id().toInt()
            FFI.setHwDecodeSurface(sessionId, display, surface, id.toLong())
            entries[key] = entry
            id
        } catch (e: Exception) {
            Log.e("HwDecodeSurface", "Failed to create surface texture: ${e.message}", e)
            -1
        }
    }

    private fun destroy(sessionId: String, display: Int) {
        val key = "$sessionId-$display"
        val entry = entries.remove(key) ?: return
        // Drop the native window first, then release the entry.
        FFI.removeHwDecodeSurface(sessionId, display)
        entry.release()
    }
}
