# The JNI entry points are resolved by name at runtime, so R8 must not rename
# or strip NativeCore or the callback interface the Rust side invokes.
-keepclasseswithmembernames class com.conquerd.client.NativeCore {
    native <methods>;
}
-keep class com.conquerd.client.NativeCore { *; }
-keep interface com.conquerd.client.NativeCore$EventSink { *; }

# kotlinx.serialization generates serializers reflectively from these.
-keepattributes *Annotation*, InnerClasses
-dontnote kotlinx.serialization.**
-keepclassmembers class com.conquerd.client.** {
    *** Companion;
}
-keepclasseswithmembers class com.conquerd.client.** {
    kotlinx.serialization.KSerializer serializer(...);
}
