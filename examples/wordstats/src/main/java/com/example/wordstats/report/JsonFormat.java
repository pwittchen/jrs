package com.example.wordstats.report;

import java.util.stream.Collectors;

/** A single-line JSON object. */
public final class JsonFormat implements ReportFormat {
    @Override
    public String name() {
        return "json";
    }

    @Override
    public String render(Report report) {
        String top = report.top().stream()
                .map(w -> "{\"word\":" + quote(w.word()) + ",\"count\":" + w.count() + "}")
                .collect(Collectors.joining(",", "[", "]"));
        return "{\"source\":" + quote(report.source())
                + ",\"totalWords\":" + report.totalWords()
                + ",\"distinctWords\":" + report.distinctWords()
                + ",\"top\":" + top + "}\n";
    }

    static String quote(String s) {
        StringBuilder out = new StringBuilder("\"");
        for (char c : s.toCharArray()) {
            switch (c) {
                case '"' -> out.append("\\\"");
                case '\\' -> out.append("\\\\");
                case '\n' -> out.append("\\n");
                case '\t' -> out.append("\\t");
                default -> {
                    if (c < 0x20) {
                        out.append(String.format("\\u%04x", (int) c));
                    } else {
                        out.append(c);
                    }
                }
            }
        }
        return out.append('"').toString();
    }
}
