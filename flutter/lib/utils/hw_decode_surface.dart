import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../common.dart';

// SurfaceTexture registration for zero-copy MediaCodec decoding (android).
class HwDecodeSurface {
  static final _channel = MethodChannel('mHwDecodeSurfaceChannel');

  /// Returns the flutter texture id, or -1 on failure.
  static Future<int> create(String sessionId, int display) async {
    if (!isAndroid) return -1;
    try {
      final id = await _channel.invokeMethod('create', {
        'sessionId': sessionId,
        'display': display,
      });
      return id is int ? id : -1;
    } catch (e) {
      debugPrint('hw decode surface create failed: $e');
      return -1;
    }
  }

  static Future<void> destroy(String sessionId, int display) async {
    if (!isAndroid) return;
    try {
      await _channel.invokeMethod('destroy', {
        'sessionId': sessionId,
        'display': display,
      });
    } catch (e) {
      debugPrint('hw decode surface destroy failed: $e');
    }
  }
}
