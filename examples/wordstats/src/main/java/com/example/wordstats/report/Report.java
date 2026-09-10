package com.example.wordstats.report;

import java.util.List;

public record Report(String source, int totalWords, int distinctWords, List<WordCount> top) {
    public Report {
        top = List.copyOf(top);
    }
}
