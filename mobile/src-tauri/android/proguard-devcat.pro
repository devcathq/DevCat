# Release builds shrink the app with R8, which removed or renamed what ML Kit (the QR code
# scanner, tauri-plugin-barcode-scanner) looks up by reflection: scanning crashed on the first
# camera frame (NullPointerException in mlkit_vision_common, from InputImage.fromMediaImage).
# Copied into the generated Android project by scripts/android-setup.sh.
-keep class com.google.mlkit.** { *; }
-keep class com.google.android.gms.internal.mlkit_vision_common.** { *; }
-keep class com.google.android.gms.internal.mlkit_vision_barcode.** { *; }
-keep class com.google.android.gms.internal.mlkit_common.** { *; }
