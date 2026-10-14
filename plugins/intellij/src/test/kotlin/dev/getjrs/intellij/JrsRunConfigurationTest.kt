package dev.getjrs.intellij

import dev.getjrs.intellij.run.JrsRunConfiguration
import org.junit.Assert.assertEquals
import org.junit.Test

class JrsRunConfigurationTest {
    @Test
    fun `the workspace members -p names are read from the arguments`() {
        assertEquals(emptyList<String>(), JrsRunConfiguration.memberArguments("--filter ^a.B\$"))
        assertEquals(listOf("app"), JrsRunConfiguration.memberArguments("-p app --method a.B#c"))
        assertEquals(
            listOf("core", "api", "web"),
            JrsRunConfiguration.memberArguments("--module core --module=api -pweb --all"),
        )
    }

    @Test
    fun `the test action runs every member's tests from the workspace root`() {
        val arguments = dev.getjrs.intellij.actions.JrsTestAction.arguments("/work/my shop")
        val parsed = com.intellij.util.execution.ParametersListUtil.parse(arguments)
        assertEquals(listOf("--manifest-path", "/work/my shop", "--all"), parsed)
        // No member is named, so the test tree reads every member's reports.
        assertEquals(emptyList<String>(), JrsRunConfiguration.memberArguments(arguments))
    }
}
