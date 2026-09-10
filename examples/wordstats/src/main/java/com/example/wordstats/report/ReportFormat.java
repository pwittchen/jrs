package com.example.wordstats.report;

import java.util.List;
import java.util.ServiceLoader;

/**
 * An output format, discovered through {@link ServiceLoader}. Implementations are listed in
 * {@code META-INF/services/com.example.wordstats.report.ReportFormat}, so a format that is
 * missing at runtime means that resource was not copied or packaged.
 */
public interface ReportFormat {
    String name();

    String render(Report report);

    static ReportFormat named(String name) {
        for (ReportFormat format : ServiceLoader.load(ReportFormat.class)) {
            if (format.name().equals(name)) {
                return format;
            }
        }
        throw new IllegalArgumentException(
                "unknown format '" + name + "', expected one of " + available());
    }

    static List<String> available() {
        return ServiceLoader.load(ReportFormat.class).stream()
                .map(provider -> provider.get().name())
                .sorted()
                .toList();
    }
}
