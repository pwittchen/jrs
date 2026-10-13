package dev.getjrs.intellij.run

import com.intellij.execution.Executor
import com.intellij.execution.Location
import com.intellij.execution.PsiLocation
import com.intellij.execution.testframework.sm.runner.SMTRunnerConsoleProperties
import com.intellij.execution.testframework.sm.runner.SMTestLocator
import com.intellij.openapi.project.Project
import com.intellij.psi.JavaPsiFacade
import com.intellij.psi.search.GlobalSearchScope

class JrsTestConsoleProperties(configuration: JrsRunConfiguration, executor: Executor) :
    SMTRunnerConsoleProperties(configuration, "jrs", executor) {

    init {
        isIdBasedTestTree = false
    }

    override fun getTestLocator(): SMTestLocator = JrsTestLocator
}

/** Resolves `java:suite://<class>` and `java:test://<class>/<method>` to the source. */
object JrsTestLocator : SMTestLocator {
    override fun getLocation(
        protocol: String,
        path: String,
        project: Project,
        scope: GlobalSearchScope,
    ): List<Location<*>> {
        val className = path.substringBefore('/')
        val psiClass = JavaPsiFacade.getInstance(project).findClass(className, scope)
            ?: JavaPsiFacade.getInstance(project).findClass(className, GlobalSearchScope.allScope(project))
            ?: return emptyList()
        if (protocol == JunitXmlReport.SUITE_PROTOCOL || '/' !in path) return listOf(PsiLocation(psiClass))
        val method = psiClass.findMethodsByName(path.substringAfter('/'), true).firstOrNull()
        return listOf(PsiLocation(method ?: psiClass))
    }
}
