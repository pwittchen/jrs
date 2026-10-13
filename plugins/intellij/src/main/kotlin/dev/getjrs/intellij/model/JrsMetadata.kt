package dev.getjrs.intellij.model

import com.google.gson.JsonArray
import com.google.gson.JsonElement
import com.google.gson.JsonObject
import com.google.gson.JsonParser

/**
 * The project model `jrs metadata` prints (SPEC §5.4). Only format version 1 is
 * understood; new keys within it are ignored, as the format allows.
 */
data class JrsMetadata(
    val version: Int,
    val jrs: String,
    val project: ProjectInfo,
    val jdk: Jdk?,
    val java: JavaSettings,
    val languages: List<LanguageInfo>,
    val main: CompileUnit,
    val test: CompileUnit,
    val targetDir: String,
    val jar: String,
    /** Null when the model came from `jrs metadata --no-deps`. */
    val classpaths: Classpaths?,
    val tasks: List<Task>,
) {
    data class ProjectInfo(
        val name: String,
        val version: String,
        val mainClass: String?,
        val root: String,
        val manifest: String,
        val lockfile: String,
    )

    data class Jdk(val home: String?, val version: Int, val javac: String, val java: String)

    data class JavaSettings(val release: Int?, val target: Int?, val encoding: String, val javacArgs: List<String>)

    data class LanguageInfo(val name: String, val version: String?, val compiler: String?)

    data class SourceDir(val language: String, val dir: String)

    /** A compile unit's directories: `main` or `test`. */
    data class CompileUnit(
        val sources: List<SourceDir>,
        val generatedSources: List<String>,
        val resources: List<String>,
        val generatedResources: List<String>,
        val output: String,
    )

    data class Entry(val coordinate: String?, val jar: String, val sources: String?) {
        /** `group:artifact` without the version, for `jrs remove`; null for a local jar. */
        val key: String?
            get() = coordinate?.split(':')?.takeIf { it.size >= 2 }?.let { "${it[0]}:${it[1]}" }

        val displayName: String
            get() = coordinate ?: jar.substringAfterLast('/').substringAfterLast('\\')
    }

    data class Classpaths(val compile: List<Entry>, val runtime: List<Entry>, val test: List<Entry>)

    data class Task(val name: String, val description: String?, val hooks: List<String>)

    companion object {
        const val FORMAT_VERSION = 1

        /** Parses `jrs metadata`'s stdout; throws [IllegalArgumentException] on anything else. */
        fun parse(json: String): JrsMetadata {
            val root = try {
                JsonParser.parseString(json).asJsonObject
            } catch (e: RuntimeException) {
                throw IllegalArgumentException("jrs metadata did not print a JSON object", e)
            }
            val version = root.int("version") ?: throw IllegalArgumentException("no `version` in jrs metadata")
            require(version == FORMAT_VERSION) {
                "jrs metadata format version $version is not supported by this plugin (expected $FORMAT_VERSION); " +
                    "update the jrs plugin"
            }
            val project = root.obj("project")
            return JrsMetadata(
                version = version,
                jrs = root.str("jrs").orEmpty(),
                project = ProjectInfo(
                    name = project.req("name"),
                    version = project.req("version"),
                    mainClass = project.str("main-class"),
                    root = project.req("root"),
                    manifest = project.req("manifest"),
                    lockfile = project.req("lockfile"),
                ),
                jdk = root.objOrNull("jdk")?.let {
                    Jdk(it.str("home"), it.int("version") ?: 0, it.req("javac"), it.req("java"))
                },
                java = root.obj("java").let {
                    JavaSettings(it.int("release"), it.int("target"), it.str("encoding") ?: "UTF-8", it.strings("javac-args"))
                },
                languages = root.array("languages").map { it.asJsonObject }.map {
                    LanguageInfo(it.req("name"), it.str("version"), it.str("compiler"))
                },
                main = unit(root.obj("main")),
                test = unit(root.obj("test")),
                targetDir = root.req("target-dir"),
                jar = root.req("jar"),
                classpaths = root.objOrNull("classpaths")?.let {
                    Classpaths(entries(it, "compile"), entries(it, "runtime"), entries(it, "test"))
                },
                tasks = root.array("tasks").map { it.asJsonObject }.map {
                    Task(it.req("name"), it.str("description"), it.strings("hooks"))
                },
            )
        }

        private fun unit(o: JsonObject) = CompileUnit(
            sources = o.array("sources").map { it.asJsonObject }.map { SourceDir(it.req("language"), it.req("dir")) },
            generatedSources = o.strings("generated-sources"),
            resources = o.strings("resources"),
            generatedResources = o.strings("generated-resources"),
            output = o.req("output"),
        )

        private fun entries(o: JsonObject, key: String): List<Entry> =
            o.array(key).map { it.asJsonObject }.map { Entry(it.str("coordinate"), it.req("jar"), it.str("sources")) }

        private fun JsonObject.value(key: String): JsonElement? = get(key)?.takeUnless { it.isJsonNull }

        private fun JsonObject.str(key: String): String? = value(key)?.asString

        private fun JsonObject.req(key: String): String =
            str(key) ?: throw IllegalArgumentException("no `$key` in jrs metadata")

        private fun JsonObject.int(key: String): Int? = value(key)?.asInt

        private fun JsonObject.objOrNull(key: String): JsonObject? = value(key)?.asJsonObject

        private fun JsonObject.obj(key: String): JsonObject =
            objOrNull(key) ?: throw IllegalArgumentException("no `$key` in jrs metadata")

        private fun JsonObject.array(key: String): JsonArray = value(key)?.asJsonArray ?: JsonArray()

        private fun JsonObject.strings(key: String): List<String> = array(key).map { it.asString }
    }
}
