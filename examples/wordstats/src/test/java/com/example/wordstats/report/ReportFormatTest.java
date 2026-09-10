package com.example.wordstats.report;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.util.List;
import org.junit.jupiter.api.Test;

class ReportFormatTest {
    private static final Report REPORT =
            new Report("a \"quoted\" source", 5, 2, List.of(new WordCount("jar", 4), new WordCount("zip", 1)));

    @Test
    void discoversBothFormatsThroughServiceLoader() {
        assertEquals(List.of("json", "text"), ReportFormat.available());
    }

    @Test
    void rejectsAnUnknownFormat() {
        IllegalArgumentException e = assertThrows(IllegalArgumentException.class, () -> ReportFormat.named("xml"));
        assertEquals("unknown format 'xml', expected one of [json, text]", e.getMessage());
    }

    @Test
    void rendersJson() {
        assertEquals(
                "{\"source\":\"a \\\"quoted\\\" source\",\"totalWords\":5,\"distinctWords\":2,"
                        + "\"top\":[{\"word\":\"jar\",\"count\":4},{\"word\":\"zip\",\"count\":1}]}\n",
                ReportFormat.named("json").render(REPORT));
    }

    @Test
    void rendersTextWithProportionalBars() {
        String expected = String.join("\n",
                "a \"quoted\" source: 5 words, 2 distinct",
                "",
                "  jar     4  " + "#".repeat(30),
                "  zip     1  " + "#".repeat(7),
                "");
        assertEquals(expected, ReportFormat.named("text").render(REPORT));
    }
}
