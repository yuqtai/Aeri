plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// 版本号和电脑版一致：取仓库根目录 Cargo.toml 里的 version（测试时可用 -PaeriVersion=x.y.z 覆盖）。
// versionCode = 主版本 * 10000 + 次版本 * 100 + 修订号，和 build.ps1 写进 Aeri.json 的算法一致
val aeriVersion: String = (project.findProperty("aeriVersion") as String?)
    ?: rootDir.parentFile.resolve("Cargo.toml").readLines()
        .first { it.startsWith("version = ") }
        .substringAfter('"').substringBefore('"')
val aeriVersionCode: Int = aeriVersion.split('.').map { part -> part.takeWhile(Char::isDigit).toInt() }
    .let { (major, minor, patch) -> major * 10000 + minor * 100 + patch }

android {
    namespace = "com.aeri.remote"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.aeri.remote"
        minSdk = 26
        targetSdk = 36
        versionCode = aeriVersionCode
        versionName = aeriVersion
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // 只给自己装，不上架：直接用调试证书签名，装上就能用
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.03.01"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.12.4")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.10.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.10.0")
    implementation("androidx.core:core-ktx:1.17.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
}
