package dev.getjrs.intellij.settings

import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.service

class JrsSettingsState : BaseState() {
    /** The `jrs` binary; empty means look it up on `PATH` and in the usual install directories. */
    var executable by string("")

    /** Delegate Build Project to `jrs build`. */
    var buildWithJrs by property(true)

    /** Load the project again without asking when `jrs.toml` or `jrs.lock` changes. */
    var autoReload by property(false)

    /** Import a project with a `jrs.toml` when it is opened, without asking. */
    var autoImport by property(false)
}

@Service(Service.Level.APP)
@State(name = "JrsSettings", storages = [Storage("jrs.xml")])
class JrsSettings : SimplePersistentStateComponent<JrsSettingsState>(JrsSettingsState()) {
    companion object {
        fun getInstance(): JrsSettings = service()
    }
}
