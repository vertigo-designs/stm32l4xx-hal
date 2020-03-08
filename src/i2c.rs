//! Inter-Integrated Circuit (I2C) bus

use cast::u8;
use crate::stm32::{I2C1, I2C2};

use crate::gpio::gpioa::{PA10, PA9};
use crate::gpio::gpiob::{PB6, PB7, PB10, PB11, PB13, PB14};
use crate::gpio::gpioc::{PC0, PC1};
use crate::gpio::{AF4, Alternate, OpenDrain, Output};
use crate::hal::blocking::i2c::{Write, WriteRead, Read};
use crate::rcc::{APB1R1, Clocks};
use crate::time::Hertz;

/// I2C error
#[derive(Debug)]
pub enum Error {
    /// Bus error
    Bus,
    /// Arbitration loss
    Arbitration,
    /// NACK
    Nack,
    // Overrun, // slave mode only
    // Pec, // SMBUS mode only
    // Timeout, // SMBUS mode only
    // Alert, // SMBUS mode only
    #[doc(hidden)]
    _Extensible,
}

#[doc(hidden)]
mod private {
    pub trait Sealed {}
}

/// SCL pin. This trait is sealed and cannot be implemented.
pub unsafe trait SclPin<I2C>: private::Sealed {}

/// SDA pin. This trait is sealed and cannot be implemented.
pub trait SdaPin<I2C>: private::Sealed {}

/// I2C peripheral operating in master mode
pub struct I2c<I2C, PINS> {
    i2c: I2C,
    pins: PINS,
}

macro_rules! pins {
    ($i2c:ident, $af:ident, SCL: [$($sck:ident),*], SCK: [$($scl:ident),*]) => {
        $(
            impl private::Sealed for $scl<Alternate<$af, Output<OpenDrain>>> {}
            impl SclPin for $scl<Alternate<$af, Output<OpenDrain>>> {}
        )*
        $(
            impl private::Sealed for $sda<Alternate<$af, Output<OpenDrain>>> {}
            impl SdaPin for $sda<Alternate<$af, Output<OpenDrain>>> {}
        )*
    }
}

macro_rules! busy_wait {
    ($i2c:expr, $flag:ident) => {
        loop {
            let isr = $i2c.isr.read();

            if isr.berr().bit_is_set() {
                return Err(Error::Bus);
            } else if isr.arlo().bit_is_set() {
                return Err(Error::Arbitration);
            } else if isr.nackf().bit_is_set() {
                return Err(Error::Nack);
            } else if isr.$flag().bit_is_set() {
                break;
            } else {
                // try again
            }
        }
    };
}

macro_rules! hal {
    ($($I2CX:ident: ($i2cX:ident, $i2cXen:ident, $i2cXrst:ident),)+) => {
        $(
            impl<SCL, SDA> I2c<$I2CX, (SCL, SDA)> {
                /// Configures the I2C peripheral to work in master mode
                pub fn $i2cX<F>(
                    i2c: $I2CX,
                    pins: (SCL, SDA),
                    freq: F,
                    clocks: Clocks,
                    apb1: &mut APB1R1,
                ) -> Self where
                    F: Into<Hertz>,
                    SCL: SclPin<$I2CX>,
                    SDA: SdaPin<$I2CX>,
                {
                    apb1.enr().modify(|_, w| w.$i2cXen().set_bit());
                    apb1.rstr().modify(|_, w| w.$i2cXrst().set_bit());
                    apb1.rstr().modify(|_, w| w.$i2cXrst().clear_bit());

                    let freq = freq.into().0;

                    assert!(freq <= 1_000_000);

                    // TODO review compliance with the timing requirements of I2C
                    // t_I2CCLK = 1 / PCLK1
                    // t_PRESC  = (PRESC + 1) * t_I2CCLK
                    // t_SCLL   = (SCLL + 1) * t_PRESC
                    // t_SCLH   = (SCLH + 1) * t_PRESC
                    //
                    // t_SYNC1 + t_SYNC2 > 4 * t_I2CCLK
                    // t_SCL ~= t_SYNC1 + t_SYNC2 + t_SCLL + t_SCLH
                    let i2cclk = clocks.pclk1().0;
                    let ratio = i2cclk / freq - 4;
                    let (presc, scll, sclh, sdadel, scldel) = if freq >= 100_000 {
                        // fast-mode or fast-mode plus
                        // here we pick SCLL + 1 = 2 * (SCLH + 1)
                        let presc = ratio / 387;

                        let sclh = ((ratio / (presc + 1)) - 3) / 3;
                        let scll = 2 * (sclh + 1) - 1;

                        let (sdadel, scldel) = if freq > 400_000 {
                            // fast-mode plus
                            let sdadel = 0;
                            let scldel = i2cclk / 4_000_000 / (presc + 1) - 1;

                            (sdadel, scldel)
                        } else {
                            // fast-mode
                            let sdadel = i2cclk / 8_000_000 / (presc + 1);
                            let scldel = i2cclk / 2_000_000 / (presc + 1) - 1;

                            (sdadel, scldel)
                        };

                        (presc, scll, sclh, sdadel, scldel)
                    } else {
                        // standard-mode
                        // here we pick SCLL = SCLH
                        let presc = ratio / 514;

                        let sclh = ((ratio / (presc + 1)) - 2) / 2;
                        let scll = sclh;

                        let sdadel = i2cclk / 2_000_000 / (presc + 1);
                        let scldel = i2cclk / 800_000 / (presc + 1) - 1;

                        (presc, scll, sclh, sdadel, scldel)
                    };

                    let presc = u8(presc).unwrap();
                    assert!(presc < 16);
                    let scldel = u8(scldel).unwrap();
                    assert!(scldel < 16);
                    let sdadel = u8(sdadel).unwrap();
                    assert!(sdadel < 16);
                    let sclh = u8(sclh).unwrap();
                    let scll = u8(scll).unwrap();

                    // Configure for "fast mode" (400 KHz)
                    i2c.timingr.write(|w| {
                        w.presc()
                            .bits(presc)
                            .scll()
                            .bits(scll)
                            .sclh()
                            .bits(sclh)
                            .sdadel()
                            .bits(sdadel)
                            .scldel()
                            .bits(scldel)
                    });

                    // Enable the peripheral
                    i2c.cr1.write(|w| w.pe().set_bit());

                    I2c { i2c, pins }
                }

                /// Releases the I2C peripheral and associated pins
                pub fn free(self) -> ($I2CX, (SCL, SDA)) {
                    (self.i2c, self.pins)
                }
            }

            impl<PINS> Write for I2c<$I2CX, PINS> {
                type Error = Error;

                fn write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), Error> {
                    // TODO support transfers of more than 255 bytes
                    assert!(bytes.len() < 256 && bytes.len() > 0);

                    // START and prepare to send `bytes`
                    self.i2c.cr2.write(|w| {
                        w.sadd()
                            .bits(addr as u16) // upto 9 bits for address
                            .rd_wrn()
                            .clear_bit()
                            .nbytes()
                            .bits(bytes.len() as u8)
                            .start()
                            .set_bit()
                            .autoend()
                            .set_bit()
                    });

                    for byte in bytes {
                        // Wait until we are allowed to send data (START has been ACKed or last byte
                        // when through)
                        busy_wait!(self.i2c, txis);

                        // put byte on the wire
                        self.i2c.txdr.write(|w| { w.txdata().bits(*byte) });
                    }

                    // automatic STOP

                    Ok(())
                }
            }

            impl<PINS> Read for I2c<$I2CX, PINS> {
                type Error = Error;

                fn read(&mut self,
                    addr: u8,
                    buffer: &mut [u8],) -> Result<(), Error> {
                    self.i2c.cr2.write(|w| {
                        w.sadd()
                            .bits(addr as u16)
                            .rd_wrn()
                            .set_bit()
                            .nbytes()
                            .bits(buffer.len() as u8)
                            .start()
                            .set_bit()
                            .autoend()
                            .set_bit()
                    });

                    for byte in buffer {
                        // Wait until we have received something
                        busy_wait!(self.i2c, rxne);

                        *byte = self.i2c.rxdr.read().rxdata().bits();
                    }

                    Ok(())
                }
            }

            impl<PINS> WriteRead for I2c<$I2CX, PINS> {
                type Error = Error;

                fn write_read(
                    &mut self,
                    addr: u8,
                    bytes: &[u8],
                    buffer: &mut [u8],
                ) -> Result<(), Error> {
                    // TODO support transfers of more than 255 bytes
                    assert!(bytes.len() < 256 && bytes.len() > 0);
                    assert!(buffer.len() < 256 && buffer.len() > 0);

                    // TODO do we have to explicitly wait here if the bus is busy (e.g. another
                    // master is communicating)?

                    // START and prepare to send `bytes`
                    self.i2c.cr2.write(|w| {
                        w.sadd()
                            .bits(addr as u16)
                            .rd_wrn()
                            .clear_bit()
                            .nbytes()
                            .bits(bytes.len() as u8)
                            .start()
                            .set_bit()
                            .autoend()
                            .clear_bit()
                    });

                    for byte in bytes {
                        // Wait until we are allowed to send data (START has been ACKed or last byte
                        // when through)
                        busy_wait!(self.i2c, txis);

                        // put byte on the wire
                        self.i2c.txdr.write(|w| { w.txdata().bits(*byte) });
                    }

                    // Wait until the last transmission is finished
                    busy_wait!(self.i2c, tc);

                    // reSTART and prepare to receive bytes into `buffer`
                    self.i2c.cr2.write(|w| {
                        w.sadd()
                            .bits(addr as u16)
                            .rd_wrn()
                            .set_bit()
                            .nbytes()
                            .bits(buffer.len() as u8)
                            .start()
                            .set_bit()
                            .autoend()
                            .set_bit()
                    });

                    for byte in buffer {
                        // Wait until we have received something
                        busy_wait!(self.i2c, rxne);

                        *byte = self.i2c.rxdr.read().rxdata().bits();
                    }

                    // automatic STOP - due to autoend

                    Ok(())
                }
            }
        )+
    }
}

hal! {
    I2C1: (i2c1, i2c1en, i2c1rst),
    I2C2: (i2c2, i2c2en, i2c2rst),
}

pins!(I2C1, AF4,
    SCL: [PA9, PB6, PB8],
    SDA: [PA10, PB7, PB9],
);

pins!(I2C2, AF4,
    SCL: [PB10, PB13],
    SDA: [PB11, PB14],
);

#[cfg(any(
    feature = "stm32l452",
))]
{
    use crate::stm32::{I2C3, I2C4};
}

#[cfg(any(
    feature = "stm32l452",
))]
hal! {
    I2C3: (i2c3, i2c3en, i2c3rst),
    I2C4: (i2c4, i2c4en, i2c4rst),
}

#[cfg(any(
    feature = "stm32l452",
))]
pins!(I2C3, AF4,
    SCL: [PA7, PC0],
    SDA: [PB4, PC1],
);

#[cfg(any(
    feature = "stm32l452",
))]
{
    pins!(I2C4, AF2,
        SCL: [PC0],
        SDA: [PC1],
    );

    pins!(I2C4, AF3,
        SCL: [PB10],
        SDA: [PB11],
    );

    pins!(I2C4, AF5,
        SCL: [PB7],
        SDA: [PB8],
    );
}
