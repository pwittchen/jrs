package dev.getjrs.intellij

import com.intellij.openapi.util.IconLoader
import javax.swing.Icon

/** The jrs logo, drawn from the repository's `logo.png` (`@2x` for HiDPI). */
object JrsIcons {
    @JvmField
    val Jrs: Icon = IconLoader.getIcon("/icons/jrs.png", JrsIcons::class.java)
}
