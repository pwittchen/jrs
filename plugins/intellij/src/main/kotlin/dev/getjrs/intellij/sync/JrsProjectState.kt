package dev.getjrs.intellij.sync

import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.StoragePathMacros
import com.intellij.openapi.components.service
import com.intellij.openapi.module.Module
import com.intellij.openapi.project.Project
import com.intellij.util.messages.Topic
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.model.JrsMetadata
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest

class JrsProjectStateData : BaseState() {
    /** The module the last sync set up; null until the project is imported. */
    var moduleName by string()

    /** SHA-256 of `jrs.toml` and `jrs.lock` as the last sync saw them. */
    var syncedDigest by string()
}

/** Whether, and from which files, this project was set up by jrs; kept in the workspace file. */
@Service(Service.Level.PROJECT)
@State(name = "JrsProject", storages = [Storage(StoragePathMacros.WORKSPACE_FILE)])
class JrsProjectState(private val project: Project) :
    SimplePersistentStateComponent<JrsProjectStateData>(JrsProjectStateData()) {

    /** The model of the last successful sync, for the tool window. Not persisted. */
    @Volatile
    var metadata: JrsMetadata? = null
        private set

    val isLinked: Boolean get() = state.moduleName != null

    fun isJrsModule(module: Module): Boolean = state.moduleName == module.name

    fun linked(moduleName: String, metadata: JrsMetadata, root: Path) {
        state.moduleName = moduleName
        state.syncedDigest = digest(root)
        this.metadata = metadata
        project.messageBus.syncPublisher(TOPIC).modelChanged(metadata)
    }

    /** True when `jrs.toml` or `jrs.lock` differ from what the last sync loaded. */
    fun isOutOfDate(): Boolean {
        if (!isLinked) return false
        val root = JrsFiles.projectRoot(project) ?: return false
        return digest(root) != state.syncedDigest
    }

    fun interface Listener {
        fun modelChanged(metadata: JrsMetadata)
    }

    companion object {
        @JvmField
        val TOPIC: Topic<Listener> = Topic.create("jrs model", Listener::class.java)

        fun getInstance(project: Project): JrsProjectState = project.service()

        fun digest(root: Path): String {
            val sha = MessageDigest.getInstance("SHA-256")
            for (name in listOf(JrsFiles.MANIFEST, JrsFiles.LOCKFILE)) {
                val file = root.resolve(name)
                sha.update(name.toByteArray())
                if (Files.isRegularFile(file)) sha.update(Files.readAllBytes(file))
                sha.update(0)
            }
            return sha.digest().joinToString("") { "%02x".format(it) }
        }
    }
}
