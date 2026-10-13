package dev.getjrs.intellij.run

import com.intellij.execution.actions.ConfigurationContext
import com.intellij.execution.actions.LazyRunConfigurationProducer
import com.intellij.execution.configurations.ConfigurationFactory
import com.intellij.openapi.module.ModuleUtilCore
import com.intellij.openapi.roots.ProjectFileIndex
import com.intellij.openapi.util.Ref
import com.intellij.psi.PsiElement
import dev.getjrs.intellij.sync.JrsProjectState
import org.jetbrains.uast.UClass
import org.jetbrains.uast.UMethod
import org.jetbrains.uast.getContainingUClass
import org.jetbrains.uast.getUastParentOfType

/**
 * `jrs test` from a test class or method in the editor or the gutter: a class
 * becomes `--filter '^<class>$'`, a method `--method <class>#<method>`.
 */
class JrsTestConfigurationProducer : LazyRunConfigurationProducer<JrsRunConfiguration>() {
    override fun getConfigurationFactory(): ConfigurationFactory = JrsConfigurationType.getInstance().factory

    override fun setupConfigurationFromContext(
        configuration: JrsRunConfiguration,
        context: ConfigurationContext,
        sourceElement: Ref<PsiElement>,
    ): Boolean {
        val target = target(context) ?: return false
        configuration.command = "test"
        configuration.arguments = target.arguments
        configuration.programArguments = ""
        configuration.name = "jrs test ${target.label}"
        return true
    }

    override fun isConfigurationFromContext(configuration: JrsRunConfiguration, context: ConfigurationContext): Boolean {
        val target = target(context) ?: return false
        return configuration.isTest && configuration.arguments == target.arguments
    }

    private data class Target(val arguments: String, val label: String)

    private fun target(context: ConfigurationContext): Target? {
        val element = context.psiLocation ?: return null
        val module = ModuleUtilCore.findModuleForPsiElement(element) ?: return null
        if (!JrsProjectState.getInstance(context.project).isJrsModule(module)) return null
        val file = element.containingFile?.virtualFile ?: return null
        if (!ProjectFileIndex.getInstance(context.project).isInTestSourceContent(file)) return null

        val method = element.getUastParentOfType(UMethod::class.java)
        val uClass = method?.getContainingUClass() ?: element.getUastParentOfType(UClass::class.java) ?: return null
        val className = uClass.qualifiedName ?: return null
        if (method != null && !method.isConstructor) {
            return Target("--method $className#${method.name}", "${uClass.name}.${method.name}")
        }
        // JUnit matches the pattern against the binary name; `.` also matches a nested class's `$`.
        return Target("--filter ^$className\$", uClass.name ?: className)
    }
}
