package dev.getjrs.intellij

import com.intellij.notification.Notification
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.text.StringUtil

/** Balloons in the `jrs` notification group; [content] is plain text, escaped here. */
object JrsNotifications {
    fun notify(project: Project?, content: String, type: NotificationType = NotificationType.INFORMATION): Notification {
        val notification = NotificationGroupManager.getInstance().getNotificationGroup("jrs")
            .createNotification("jrs", StringUtil.escapeXmlEntities(content).replace("\n", "<br>"), type)
        notification.notify(project)
        return notification
    }

    fun error(project: Project?, content: String): Notification = notify(project, content, NotificationType.ERROR)
}
