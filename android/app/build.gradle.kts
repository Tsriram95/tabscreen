import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Release signing: environment variables (CI) or ~/.config/tabscreen/keystore.properties (local).
// Without either, the release build is debug-signed so it still installs for local testing.
val signing: Map<String, String>? = run {
    val env = System.getenv()
    if (env["TABSCREEN_KEYSTORE"] != null) {
        mapOf(
            "storeFile" to env.getValue("TABSCREEN_KEYSTORE"),
            "storePassword" to env.getValue("TABSCREEN_KEYSTORE_PASSWORD"),
            "keyAlias" to env.getValue("TABSCREEN_KEY_ALIAS"),
            "keyPassword" to env.getValue("TABSCREEN_KEY_PASSWORD"),
        )
    } else {
        val f = File(System.getProperty("user.home"), ".config/tabscreen/keystore.properties")
        if (f.exists()) Properties().apply { f.inputStream().use { load(it) } }.entries.associate { it.key.toString() to it.value.toString() } else null
    }
}

android {
    namespace = "dev.tabscreen"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.tabscreen"
        minSdk = 30
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    signingConfigs {
        if (signing != null) {
            create("release") {
                storeFile = file(signing.getValue("storeFile"))
                storePassword = signing.getValue("storePassword")
                keyAlias = signing.getValue("keyAlias")
                keyPassword = signing.getValue("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = if (signing != null) signingConfigs.getByName("release") else signingConfigs.getByName("debug")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("com.google.android.material:material:1.12.0")
}
