import java.io.File
import java.security.SecureRandom
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// 版本号和电脑版一致：取仓库根目录 Cargo.toml 里的 version（测试时可用 -PaeriVersion=x.y.z 覆盖）。
// versionCode = 主版本 * 10000 + 次版本 * 100 + 修订号，和 build.ps1 写进 Aeri.json 的算法保持一致；
// 预发布版升正式版（如 0.4.9-rc.1 -> 0.4.9）由手机端结合 versionName 差异识别更新。
val aeriVersion: String = (project.findProperty("aeriVersion") as String?)
    ?: rootDir.parentFile.resolve("Cargo.toml").readLines()
        .first { it.startsWith("version = ") }
        .substringAfter('"').substringBefore('"')
val aeriVersionParts: List<Int> = aeriVersion.substringBefore('-').substringBefore('+').split('.')
    .map { part -> part.takeWhile(Char::isDigit).toIntOrNull() ?: 0 }
val aeriVersionCode: Int = aeriVersionParts.getOrElse(0) { 0 } * 10000 +
    aeriVersionParts.getOrElse(1) { 0 } * 100 +
    aeriVersionParts.getOrElse(2) { 0 }

data class SigningSpec(
    val storeFile: File,
    val storePassword: String,
    val keyAlias: String,
    val keyPassword: String,
)

fun resolveReleaseSigning(): SigningSpec? {
    val propsFile = rootDir.resolve("keystore.properties")
    val props = Properties().apply {
        if (propsFile.isFile) propsFile.inputStream().use { load(it) }
    }
    fun read(envKey: String, propKey: String): String? =
        System.getenv(envKey)?.trim()?.takeIf { it.isNotEmpty() }
            ?: props.getProperty(propKey)?.trim()?.takeIf { it.isNotEmpty() }
            ?: props.getProperty(envKey)?.trim()?.takeIf { it.isNotEmpty() }

    val configuredPath = read("AERI_KEYSTORE_FILE", "storeFile")
    if (configuredPath != null) {
        val file = File(configuredPath).let { if (it.isAbsolute) it else rootDir.resolve(configuredPath) }
        val storePass = read("AERI_KEYSTORE_PASSWORD", "storePassword")
        val alias = read("AERI_KEY_ALIAS", "keyAlias")
        val keyPass = read("AERI_KEY_PASSWORD", "keyPassword") ?: storePass
        if (file.isFile && storePass != null && alias != null && keyPass != null) {
            return SigningSpec(file, storePass, alias, keyPass)
        }
        logger.warn("指定的签名库不可用或缺少密码/别名（${file.absolutePath}），尝试回退本机持久发布证书")
    }

    // 未指定环境变量或 keystore.properties 时，在用户配置目录持久保存一份 30 年有效期的正式证书，
    // 既避免提交私钥到仓库，又避免临时 debug.keystore 过期清理后覆盖更新失败丢配对。
    val isWindows = System.getProperty("os.name").orEmpty().startsWith("Windows", ignoreCase = true)
    val configDir = if (isWindows) {
        val appData = System.getenv("APPDATA")?.takeIf { it.isNotBlank() }
            ?: File(System.getProperty("user.home"), "AppData/Roaming").path
        File(appData, "VoiceAssistant")
    } else {
        File(System.getProperty("user.home"), ".config/aeri")
    }
    val jksFile = File(configDir, "android-release.jks")
    val metaFile = File(configDir, "android-release.properties")
    val meta = Properties().apply {
        if (metaFile.isFile) runCatching { metaFile.inputStream().use { load(it) } }
    }
    val alias = meta.getProperty("keyAlias")?.takeIf { it.isNotBlank() } ?: "aeri"
    val existingPass = meta.getProperty("storePassword")?.takeIf { it.isNotBlank() }
    if (jksFile.isFile && existingPass != null) {
        val keyPass = meta.getProperty("keyPassword")?.takeIf { it.isNotBlank() } ?: existingPass
        return SigningSpec(jksFile, existingPass, alias, keyPass)
    }
    if (jksFile.isFile) {
        return SigningSpec(jksFile, "aeri-release", alias, "aeri-release")
    }

    return runCatching {
        configDir.mkdirs()
        val alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789"
        val rng = SecureRandom()
        val password = buildString(24) { repeat(24) { append(alphabet[rng.nextInt(alphabet.length)]) } }
        val exe = if (isWindows) "keytool.exe" else "keytool"
        val keytool = File(System.getProperty("java.home"), "bin/$exe")
            .takeIf { it.isFile }?.absolutePath ?: exe
        val proc = ProcessBuilder(
            keytool,
            "-genkeypair",
            "-keystore", jksFile.absolutePath,
            "-storetype", "PKCS12",
            "-storepass", password,
            "-keypass", password,
            "-alias", alias,
            "-keyalg", "RSA",
            "-keysize", "2048",
            "-validity", "10950",
            "-dname", "CN=Aeri, OU=Desktop, O=Aeri, C=CN",
        ).redirectErrorStream(true).start()
        val output = proc.inputStream.bufferedReader().readText()
        val exit = proc.waitFor()
        check(exit == 0 && jksFile.isFile) { "keytool 退出码 $exit: $output" }
        Properties().apply {
            setProperty("storePassword", password)
            setProperty("keyAlias", alias)
            setProperty("keyPassword", password)
            metaFile.outputStream().use { store(it, "Aeri Android Release Signing") }
        }
        SigningSpec(jksFile, password, alias, password)
    }.onFailure {
        logger.warn("自动生成持久发布证书失败，降级使用 debug 签名：${it.message}")
    }.getOrNull()
}

val releaseSigning: SigningSpec? = resolveReleaseSigning()

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

    val customReleaseSigning = releaseSigning?.let { spec ->
        signingConfigs.create("release") {
            storeFile = spec.storeFile
            storePassword = spec.storePassword
            keyAlias = spec.keyAlias
            keyPassword = spec.keyPassword
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = customReleaseSigning ?: signingConfigs.getByName("debug")
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
