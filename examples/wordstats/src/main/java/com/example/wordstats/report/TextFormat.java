package com.example.wordstats.report;

import org.apache.commons.lang3.StringUtils;

/** A plain-text table with a proportional bar per word. */
public final class TextFormat implements ReportFormat {
    private static final int BAR_WIDTH = 30;

    @Override
    public String name() {
        return "text";
    }

    @Override
    public String render(Report report) {
        StringBuilder out = new StringBuilder()
                .append(report.source()).append(": ")
                .append(report.totalWords()).append(" words, ")
                .append(report.distinctWords()).append(" distinct\n\n");
        if (report.top().isEmpty()) {
            return out.toString();
        }

        int wordWidth = report.top().stream().mapToInt(w -> w.word().length()).max().orElse(0);
        int highest = report.top().get(0).count();
        for (WordCount entry : report.top()) {
            int bar = Math.max(1, entry.count() * BAR_WIDTH / highest);
            out.append("  ")
                    .append(StringUtils.rightPad(entry.word(), wordWidth))
                    .append("  ")
                    .append(StringUtils.leftPad(Integer.toString(entry.count()), 4))
                    .append("  ")
                    .append(StringUtils.repeat('#', bar))
                    .append('\n');
        }
        return out.toString();
    }
}
