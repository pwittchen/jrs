package dev.getjrs.intellij.run

import com.intellij.openapi.options.SettingsEditor
import com.intellij.openapi.ui.ComboBox
import com.intellij.ui.RawCommandLineEditor
import com.intellij.util.ui.FormBuilder
import javax.swing.JComponent

class JrsSettingsEditor : SettingsEditor<JrsRunConfiguration>() {
    private val command = ComboBox(COMMANDS).apply { isEditable = true }
    private val arguments = RawCommandLineEditor()
    private val programArguments = RawCommandLineEditor()

    override fun resetEditorFrom(configuration: JrsRunConfiguration) {
        command.selectedItem = configuration.command
        arguments.text = configuration.arguments
        programArguments.text = configuration.programArguments
    }

    override fun applyEditorTo(configuration: JrsRunConfiguration) {
        configuration.command = (command.editor.item ?: command.selectedItem)?.toString()?.trim().orEmpty()
        configuration.arguments = arguments.text
        configuration.programArguments = programArguments.text
    }

    override fun createEditor(): JComponent = FormBuilder.createFormBuilder()
        .addLabeledComponent("Command:", command)
        .addLabeledComponent("Arguments:", arguments)
        .addTooltip("Flags after the command, such as --filter Foo for test or the task's name for task")
        .addLabeledComponent("Program arguments:", programArguments)
        .addTooltip("Passed after -- : to the program for run, to the task's command line for task")
        .addComponentFillVertically(javax.swing.JPanel(), 0)
        .panel

    companion object {
        val COMMANDS = arrayOf(
            "build", "test", "run", "package", "task", "doc", "clean",
            "tree", "classpath", "outdated", "update", "verify", "audit", "licenses", "fetch",
        )
    }
}
