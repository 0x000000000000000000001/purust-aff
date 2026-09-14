module Test.ErrorReporting where

import Prelude
import Control.Monad.Error.Class (catchError, throwError)
import Data.Time.Duration (Milliseconds(..))
import Effect (Effect)
import Effect.Aff (Aff, delay, finally, forkAff, launchAff_, never)
import Effect.Class (liftEffect)
import Effect.Console (log)
import Effect.Exception (error, errorWithName, throwException)

foreign import nativePanic :: Effect Unit

runScenario :: Int -> Effect Unit
runScenario scenario = case scenario of
  0 -> launchAff_ $ liftEffect $ log "success"
  1 -> launchAff_ $ catchError
    (throwError $ error "handled Aff failure")
    (const $ liftEffect $ log "handled Aff")
  2 -> launchAff_ $ finally (liftEffect $ log "finalized Aff") $
    throwError $ errorWithName "échec 🚀 漢字" "ErreurΩ"
  3 -> launchAff_ $ finally (liftEffect $ log "finalized Effect") $
    liftEffect $ throwException $ errorWithName "effet 🌍" "ErreurEffet"
  4 -> do
    launchAff_ $ void $ forkAff do
      delay (Milliseconds 20.0)
      liftEffect $ log "child finished"
    throwException $ error "synchronous main failure"
  5 -> nativePanic
  6 -> launchAff_ do
    void $ forkAff (never :: Aff Unit)
    liftEffect nativePanic
  7 -> launchAff_ $ catchError
    (liftEffect $ throwException $ error "handled Effect failure")
    (const $ liftEffect $ log "handled Effect")
  _ -> launchAff_ $ throwError $ error "unknown error-reporting scenario"
