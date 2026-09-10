package com.example.cart

import spock.lang.Specification

class DiscountSpec extends Specification {
    def "#percent% off #subtotal is #off"() {
        expect:
        Discount.percent(percent).cents(new Cart(), subtotal) == off

        where:
        percent | subtotal || off
        0       | 1000     || 0
        10      | 1000     || 100
        15      | 999      || 149
        100     | 1234     || 1234
    }

    def "every third unit of #quantity is free"() {
        given:
        def cart = new Cart().add("a", 200, quantity)

        expect:
        Discount.everyNthFree(3).cents(cart, cart.subtotalCents()) == free

        where:
        quantity || free
        2        || 0
        3        || 200
        7        || 400
    }

    def "a fixed amount comes off #subtotal only over the threshold"() {
        expect:
        Discount.amountOver(5000, 700).cents(new Cart(), subtotal) == off

        where:
        subtotal || off
        4999     || 0
        5000     || 700
        9000     || 700
    }

    def "a percentage is between 0 and 100"() {
        when:
        Discount.percent(101)

        then:
        def e = thrown(IllegalArgumentException)
        e.message == "not a percentage: 101"
    }
}
