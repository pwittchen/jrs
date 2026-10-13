package dev.getjrs.intellij.run

import com.intellij.execution.configurations.ConfigurationFactory
import com.intellij.execution.configurations.ConfigurationTypeBase
import com.intellij.execution.configurations.ConfigurationTypeUtil
import com.intellij.execution.configurations.RunConfiguration
import com.intellij.openapi.components.BaseState
import com.intellij.openapi.project.Project
import dev.getjrs.intellij.JrsIcons

class JrsConfigurationType : ConfigurationTypeBase(ID, "jrs", "Runs a jrs command", JrsIcons.Jrs) {
    init {
        addFactory(Factory(this))
    }

    val factory: ConfigurationFactory get() = configurationFactories.single()

    class Factory(type: JrsConfigurationType) : ConfigurationFactory(type) {
        override fun getId(): String = ID

        override fun createTemplateConfiguration(project: Project): RunConfiguration =
            JrsRunConfiguration(project, this, "jrs")

        override fun getOptionsClass(): Class<out BaseState> = JrsRunConfigurationOptions::class.java
    }

    companion object {
        const val ID = "JrsRunConfiguration"

        fun getInstance(): JrsConfigurationType = ConfigurationTypeUtil.findConfigurationType(JrsConfigurationType::class.java)
    }
}
