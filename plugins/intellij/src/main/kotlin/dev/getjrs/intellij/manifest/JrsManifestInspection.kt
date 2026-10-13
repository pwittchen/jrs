package dev.getjrs.intellij.manifest

import com.intellij.codeInspection.InspectionManager
import com.intellij.codeInspection.LocalInspectionTool
import com.intellij.codeInspection.ProblemDescriptor
import com.intellij.codeInspection.ProblemHighlightType
import com.intellij.openapi.util.TextRange
import com.intellij.psi.PsiFile
import dev.getjrs.intellij.JrsFiles

/** The unknown tables and keys the manifest parser would warn about and ignore. */
class JrsManifestInspection : LocalInspectionTool() {
    override fun checkFile(file: PsiFile, manager: InspectionManager, isOnTheFly: Boolean): Array<ProblemDescriptor>? {
        if (file.name != JrsFiles.MANIFEST) return null
        return JrsManifestSchema.problems(file.text).map { problem ->
            manager.createProblemDescriptor(
                file,
                TextRange(problem.start, problem.end),
                problem.message,
                ProblemHighlightType.GENERIC_ERROR_OR_WARNING,
                isOnTheFly,
            )
        }.toTypedArray()
    }
}
