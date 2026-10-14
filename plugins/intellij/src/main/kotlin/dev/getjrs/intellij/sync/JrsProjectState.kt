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

    /** In a workspace, every member's module the last sync set up. */
    var memberModules by list<String>()

    /** In a workspace, every member's root directory, whose `jrs.toml` a change to reloads. */
    var memberRoots by list<String>()

    /** In a workspace, its root directory, whose `jrs.toml` and `jrs.lock` a change to reloads. */
    var workspaceRoot by string()

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

    /** In a workspace, every member's model, in the workspace's order; else empty. Not persisted. */
    @Volatile
    var members: List<JrsMetadata> = emptyList()
        private set

    /** Whether the project is a workspace of several members. */
    val isWorkspace: Boolean get() = state.memberModules.isNotEmpty()

    /** The member a module is, by name, in a workspace; null otherwise. */
    fun memberOf(module: Module): String? = module.name.takeIf { isWorkspace && it in state.memberModules }

    /** A member's model, by its project name. */
    fun member(name: String): JrsMetadata? = members.firstOrNull { it.project.name == name }

    val isLinked: Boolean get() = state.moduleName != null

    fun isJrsModule(module: Module): Boolean =
        state.moduleName == module.name || module.name in state.memberModules

    /**
     * Records a sync: [moduleName] and [metadata] are the project's, or in a
     * workspace the member the tool window shows; [members] are every member's
     * modules.
     */
    fun linked(
        moduleName: String,
        metadata: JrsMetadata,
        root: Path,
        members: List<String> = emptyList(),
        models: List<JrsMetadata> = emptyList(),
        workspaceRoot: String? = null,
    ) {
        state.workspaceRoot = workspaceRoot
        state.moduleName = moduleName
        state.memberModules = members.toMutableList()
        state.memberRoots = models.map { it.project.root }.toMutableList()
        state.syncedDigest = digest(root, watched())
        this.members = models
        this.metadata = metadata
        project.messageBus.syncPublisher(TOPIC).modelChanged(metadata)
    }

    /** True when `jrs.toml` or `jrs.lock` differ from what the last sync loaded. */
    fun isOutOfDate(): Boolean {
        if (!isLinked) return false
        val root = JrsFiles.projectRoot(project) ?: return false
        return digest(root, watched()) != state.syncedDigest
    }

    /** The workspace's root and its members' roots, whose manifests the digest covers. */
    private fun watched(): List<String> = listOfNotNull(state.workspaceRoot) + state.memberRoots

    fun interface Listener {
        fun modelChanged(metadata: JrsMetadata)
    }

    companion object {
        @JvmField
        val TOPIC: Topic<Listener> = Topic.create("jrs model", Listener::class.java)

        fun getInstance(project: Project): JrsProjectState = project.service()

        /** SHA-256 of `jrs.toml` and `jrs.lock` at [root], and of each workspace member's `jrs.toml`. */
        fun digest(root: Path, memberRoots: List<String> = emptyList()): String {
            val sha = MessageDigest.getInstance("SHA-256")
            val files = (
                listOf(root.resolve(JrsFiles.MANIFEST), root.resolve(JrsFiles.LOCKFILE)) +
                    memberRoots.flatMap { listOf(Path.of(it).resolve(JrsFiles.MANIFEST), Path.of(it).resolve(JrsFiles.LOCKFILE)) }
                ).distinct()
            for (file in files) {
                sha.update(file.toString().toByteArray())
                if (Files.isRegularFile(file)) sha.update(Files.readAllBytes(file))
                sha.update(0)
            }
            return sha.digest().joinToString("") { "%02x".format(it) }
        }
    }
}
