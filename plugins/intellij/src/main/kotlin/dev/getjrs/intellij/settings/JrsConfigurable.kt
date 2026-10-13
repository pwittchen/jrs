package dev.getjrs.intellij.settings

import com.intellij.openapi.fileChooser.FileChooserDescriptorFactory
import com.intellij.openapi.options.BoundConfigurable
import com.intellij.openapi.ui.DialogPanel
import com.intellij.ui.dsl.builder.bindSelected
import com.intellij.ui.dsl.builder.bindText
import com.intellij.ui.dsl.builder.columns
import com.intellij.ui.dsl.builder.COLUMNS_LARGE
import com.intellij.ui.dsl.builder.panel
import dev.getjrs.intellij.JrsCli

/** Settings | Build, Execution, Deployment | Build Tools | jrs. */
class JrsConfigurable : BoundConfigurable("jrs") {
    override fun createPanel(): DialogPanel {
        val state = JrsSettings.getInstance().state
        return panel {
            row("jrs executable:") {
                textFieldWithBrowseButton(
                    FileChooserDescriptorFactory.createSingleFileNoJarsDescriptor().withTitle("jrs Executable"),
                ).columns(COLUMNS_LARGE)
                    .bindText({ state.executable.orEmpty() }, { state.executable = it.trim() })
                    .comment("Leave empty to use <code>${JrsCli.findOnPath() ?: "jrs"}</code> from PATH")
            }
            row {
                checkBox("Build the project with jrs (Build Project runs jrs build)")
                    .bindSelected({ state.buildWithJrs }, { state.buildWithJrs = it })
            }
            row {
                checkBox("Import projects with a jrs.toml when they are opened")
                    .bindSelected({ state.autoImport }, { state.autoImport = it })
            }
            row {
                checkBox("Reload the project when jrs.toml or jrs.lock changes")
                    .bindSelected({ state.autoReload }, { state.autoReload = it })
            }
        }
    }
}
