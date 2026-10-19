// as4tainer JVM side: runs RPG on Jariko, with file I/O routed to as4tainer core.
plugins {
    kotlin("jvm") version "2.1.0"
    application
}

group = "io.as4tainer"
version = "0.1.0"

val jarikoVersion: String by project

repositories {
    mavenCentral()
    // Jariko's own build resolves a few dependencies (e.g. com.github.ziggy42:kolor)
    // from these repositories; they are not on Maven Central.
    maven("https://repo.smeup.cloud/nexus/repository/public/")
    maven("https://jcenter.bintray.com")
    maven("https://jitpack.io")
}

dependencies {
    implementation("io.github.smeup.jariko:rpgJavaInterpreter-core:$jarikoVersion")
    testImplementation(kotlin("test"))
}

// No toolchain requirement: build with whatever JDK (11 or newer) runs Gradle,
// and emit Java 11 bytecode, the same target Jariko uses.
java {
    sourceCompatibility = JavaVersion.VERSION_11
    targetCompatibility = JavaVersion.VERSION_11
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_11)
    }
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(11)
}

application {
    mainClass.set("io.as4tainer.jvm.Launcher")
    applicationName = "as4tainer-jvm"
}

// Self-contained jar for the container image: java -jar as4tainer-jvm.jar
tasks.register<Jar>("fatJar") {
    group = "build"
    description = "Builds as4tainer-jvm.jar with all dependencies."
    archiveFileName.set("as4tainer-jvm.jar")
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    manifest { attributes["Main-Class"] = "io.as4tainer.jvm.Launcher" }
    from(sourceSets.main.get().output)
    dependsOn(configurations.runtimeClasspath)
    from({ configurations.runtimeClasspath.get().filter { it.name.endsWith("jar") }.map { zipTree(it) } }) {
        exclude("META-INF/*.SF", "META-INF/*.DSA", "META-INF/*.RSA")
    }
}

tasks.test {
    useJUnitPlatform()
}
