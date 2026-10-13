package dev.getjrs.intellij.manifest

import com.intellij.codeInsight.completion.CompletionContributor
import com.intellij.codeInsight.completion.CompletionParameters
import com.intellij.codeInsight.completion.CompletionResultSet
import com.intellij.codeInsight.lookup.LookupElementBuilder
import com.intellij.openapi.project.DumbAware
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.JrsIcons

/** Table names after `[` and key names at the start of a line in `jrs.toml`. */
class JrsManifestCompletionContributor : CompletionContributor(), DumbAware {
    override fun fillCompletionVariants(parameters: CompletionParameters, result: CompletionResultSet) {
        if (parameters.originalFile.name != JrsFiles.MANIFEST) return
        val text = parameters.editor.document.charsSequence
        val offset = minOf(parameters.offset, text.length)
        val lineStart = text.lastIndexOf('\n', offset - 1) + 1
        val completion = JrsManifestSchema.completion(
            text.subSequence(0, lineStart),
            text.subSequence(lineStart, offset).toString(),
        ) ?: return
        val matching = result.withPrefixMatcher(completion.prefix)
        for (candidate in completion.candidates) {
            matching.addElement(
                LookupElementBuilder.create(candidate).withIcon(JrsIcons.Jrs).withTypeText(completion.table),
            )
        }
    }
}

private fun CharSequence.lastIndexOf(c: Char, from: Int): Int {
    var i = minOf(from, length - 1)
    while (i >= 0) {
        if (this[i] == c) return i
        i--
    }
    return -1
}
