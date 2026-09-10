package com.example.orders;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.util.List;
import org.junit.jupiter.api.Test;

/** A Java test in the Kotlin test unit: compiled by javac after kotlinc. */
class LegacyPricingTest {
    @Test
    void formatsCents() {
        assertEquals("12.05", LegacyPricing.format(1205));
        assertEquals("0.07", LegacyPricing.format(7));
    }

    @Test
    void discountsBulkOrdersOnly() {
        assertEquals(0, LegacyPricing.discountCents(9, 1000));
        assertEquals(50, LegacyPricing.discountCents(10, 1000));
    }

    @Test
    void readsAKotlinOrder() {
        Order order = new Order(List.of(new LineItem("a", 10)));
        String newline = System.lineSeparator();
        assertEquals(
                "10 units in 1 lines" + newline + "bulk discount -0.50" + newline + "total 9.50",
                LegacyPricing.receipt(order, 1000));
    }
}
