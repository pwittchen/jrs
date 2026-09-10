package com.example.wordstats;

import static org.junit.jupiter.api.Assertions.assertEquals;

import com.example.wordstats.report.WordCount;
import java.util.List;
import java.util.Set;
import org.junit.jupiter.api.Test;

class WordCounterTest {
    @Test
    void countsCaseInsensitively() {
        WordCounter counter = new WordCounter(Set.of());
        counter.accept("Build build BUILD");
        assertEquals(List.of(new WordCount("build", 3)), counter.top(10));
    }

    @Test
    void foldsAccentsTogether() {
        WordCounter counter = new WordCounter(Set.of());
        counter.accept("café cafe Café");
        assertEquals(List.of(new WordCount("cafe", 3)), counter.top(10));
    }

    @Test
    void skipsStopWordsButCountsThemNowhere() {
        WordCounter counter = new WordCounter(Set.of("the"));
        counter.accept("the jar and the manifest");
        assertEquals(3, counter.totalWords());
        assertEquals(3, counter.distinctWords());
    }

    @Test
    void keepsApostrophesInsideWordsOnly() {
        WordCounter counter = new WordCounter(Set.of());
        counter.accept("don't 'quoted'");
        assertEquals(List.of(new WordCount("don't", 1), new WordCount("quoted", 1)), counter.top(10));
    }

    @Test
    void ordersByCountThenAlphabetically() {
        WordCounter counter = new WordCounter(Set.of());
        counter.accept("pear apple pear fig apple pear");
        assertEquals(
                List.of(new WordCount("pear", 3), new WordCount("apple", 2), new WordCount("fig", 1)),
                counter.top(10));
        assertEquals(List.of(new WordCount("pear", 3)), counter.top(1));
    }

    @Test
    void loadsTheBundledStopWords() throws Exception {
        WordCounter counter = WordCounter.withDefaultStopWords();
        counter.accept("the and of jrs");
        assertEquals(List.of(new WordCount("jrs", 1)), counter.top(10));
    }
}
