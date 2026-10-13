package dev.getjrs.intellij

import dev.getjrs.intellij.build.CompilerDiagnostics
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class CompilerDiagnosticsTest {
    @Test
    fun javac() {
        val d = CompilerDiagnostics.parse("/work/app/src/main/java/App.java:12: error: cannot find symbol")!!
        assertEquals(CompilerDiagnostics.Diagnostic("/work/app/src/main/java/App.java", 12, null, true, "cannot find symbol"), d)
        assertEquals(false, CompilerDiagnostics.parse("/w/App.java:3: warning: [deprecation] x")!!.error)
    }

    @Test
    fun kotlinc() {
        val d = CompilerDiagnostics.parse("e: file:///work/app/src/main/kotlin/App.kt:12:5 Unresolved reference 'x'.")!!
        assertEquals(CompilerDiagnostics.Diagnostic("/work/app/src/main/kotlin/App.kt", 12, 5, true, "Unresolved reference 'x'."), d)
    }

    @Test
    fun `other lines are not diagnostics`() {
        assertNull(CompilerDiagnostics.parse("   Compiling app v1.0.0 (3 sources)"))
        assertNull(CompilerDiagnostics.parse("        symbol:   variable x"))
    }
}
